import { invoke } from "@tauri-apps/api/core";
import {
  Activity,
  CheckCircle2,
  CircleAlert,
  ClipboardCopy,
  ExternalLink,
  Eye,
  EyeOff,
  Info,
  KeyRound,
  Moon,
  Network,
  Play,
  RefreshCw,
  Rocket,
  Settings2,
  ShieldCheck,
  Stethoscope,
  Sun,
  Wrench,
  type LucideIcon,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type ChangeEvent } from "react";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { canLaunch, launchCommand } from "../uapi-launch-policy";
import { MANAGER_ACTIVATION_POLL_MS, handleManagerActivation } from "../uapi-manager-activation";
import { distribution } from "./distribution";
import "./uapi.css";

type Route = "overview" | "connection" | "maintenance" | "about";
type Theme = "dark" | "light";
type BusyAction = "validate" | "configure" | "refresh" | "status" | "switchMode" | "launch" | "repair" | "diagnostics" | null;
type UapiConnectionMode = "uapi" | "official";

type CommandResult<T> = T & {
  status: string;
  message: string;
};

type PathState = {
  status: string;
  path: string | null;
};

type LaunchStatus = {
  status: string;
  message: string;
  started_at_ms: number;
  debug_port: number | null;
  helper_port: number | null;
  codex_app: string | null;
};

type OverviewResult = CommandResult<{
  codex_app: PathState;
  codex_version: string | null;
  silent_shortcut: PathState;
  management_shortcut: PathState;
  latest_launch: LaunchStatus | null;
  current_version: string;
  update_status: string;
  settings_path: string;
  logs_path: string;
}>;

type UapiStatus = {
  configured: boolean;
  active: boolean;
  connectionMode: UapiConnectionMode;
  uapiReady: boolean;
  officialLoginSaved: boolean;
  officialAuthenticated: boolean;
  officialAccountLabel: string | null;
  credentialStoreAvailable: boolean;
  credentialStoreMessage: string;
  providerId: string;
  baseUrl: string;
  currentModel: string;
  compatibleModels: string[];
  modelCount: number;
  apiKeyMasked: string;
  configPath: string;
};

type UapiModelInfo = {
  id: string;
  supportedEndpointTypes: string[];
  compatible: boolean;
  reason: string;
};

type UapiModelDiscovery = CommandResult<{
  endpoint: string;
  models: UapiModelInfo[];
  compatibleModels: string[];
  filteredModels: string[];
}>;

type UapiApplyResult = CommandResult<{
  configured: boolean;
  currentModel: string;
  compatibleModels: string[];
  filteredModels: string[];
  backupPath: string | null;
  configPath: string;
}>;

type UapiModeSwitchResult = CommandResult<{
  connectionMode: UapiConnectionMode;
  configured: boolean;
  officialLoginSaved: boolean;
  officialAuthenticated: boolean;
  backupPath: string | null;
  configPath: string;
  restartRequired: boolean;
}>;

type DiagnosticsResult = CommandResult<{ report: string }>;

type VersionResult = CommandResult<{ version: string }>;

type Notice = {
  kind: "ok" | "error" | "info";
  text: string;
};

const navItems: Array<{ id: Route; label: string; detail: string; icon: LucideIcon }> = [
  { id: "overview", label: "概览", detail: "运行状态", icon: Activity },
  { id: "connection", label: "连接设置", detail: "服务密钥", icon: KeyRound },
  { id: "maintenance", label: "检查与修复", detail: "诊断工具", icon: Stethoscope },
  { id: "about", label: "关于", detail: "版本与帮助", icon: Info },
];

const RESTART_REQUIRED_STORAGE_KEY = "uapi-connect-restart-required";

class StaleRefreshError extends Error {
  constructor() {
    super("state refresh was superseded");
    this.name = "StaleRefreshError";
  }
}

export function UapiApp() {
  const [route, setRoute] = useState<Route>(() =>
    new URLSearchParams(window.location.search).get("configure") === "1" ? "connection" : "overview",
  );
  const [theme, setTheme] = useState<Theme>(() =>
    window.localStorage.getItem("codex-plus-theme") === "light" ? "light" : "dark",
  );
  const [overview, setOverview] = useState<OverviewResult | null>(null);
  const [status, setStatus] = useState<UapiStatus | null>(null);
  const [statusLoadFailed, setStatusLoadFailed] = useState(false);
  const [version, setVersion] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [busy, setBusy] = useState<BusyAction>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [discovery, setDiscovery] = useState<UapiModelDiscovery | null>(null);
  const [restartRequired, setRestartRequired] = useState(
    () => window.localStorage.getItem(RESTART_REQUIRED_STORAGE_KEY) === "1",
  );
  const refreshGeneration = useRef(0);
  const interactionGeneration = useRef(0);

  useEffect(() => {
    document.documentElement.classList.toggle("light", theme === "light");
    document.documentElement.classList.toggle("dark", theme === "dark");
    window.localStorage.setItem("codex-plus-theme", theme);
  }, [theme]);

  useEffect(() => {
    if (restartRequired) {
      window.localStorage.setItem(RESTART_REQUIRED_STORAGE_KEY, "1");
    } else {
      window.localStorage.removeItem(RESTART_REQUIRED_STORAGE_KEY);
    }
  }, [restartRequired]);

  const refreshState = useCallback(async () => {
    const generation = ++refreshGeneration.current;
    const isLatest = () => refreshGeneration.current === generation;
    // 连接状态决定能否启动，是关键结果；概览和版本只是辅助信息，任一
    // 读取失败都不应把已配置用户留在“未配置”的假状态。
    const auxiliaryResults = Promise.allSettled([
      invoke<OverviewResult>("load_overview"),
      invoke<VersionResult>("backend_version"),
    ]);
    let statusResult: CommandResult<UapiStatus>;
    try {
      statusResult = await invoke<CommandResult<UapiStatus>>("uapi_status");
    } catch (error) {
      if (!isLatest()) {
        throw new StaleRefreshError();
      }
      setStatusLoadFailed(true);
      throw error;
    }
    if (isLatest()) {
      setStatus(statusResult);
      setStatusLoadFailed(false);
      if (!canLaunch(statusResult)) {
        setRoute((current) => current === "overview" ? "connection" : current);
      }
    }
    const [overviewResult, versionResult] = await auxiliaryResults;
    if (!isLatest()) {
      throw new StaleRefreshError();
    }
    if (overviewResult.status === "fulfilled") {
      setOverview(overviewResult.value);
    }
    if (versionResult.status === "fulfilled") {
      setVersion(versionResult.value.version);
    }
    return statusResult;
  }, []);

  const refreshLatestState = useCallback(async () => {
    while (true) {
      try {
        return await refreshState();
      } catch (error) {
        if (!(error instanceof StaleRefreshError)) {
          throw error;
        }
      }
    }
  }, [refreshState]);

  useEffect(() => {
    const interaction = interactionGeneration.current;
    void refreshState().catch((error) => {
      if (error instanceof StaleRefreshError || interactionGeneration.current !== interaction) return;
      setNotice({ kind: "error", text: friendlyError(error) });
    });
  }, [refreshState]);

  useEffect(() => {
    let disposed = false;
    let consuming = false;
    const consumePendingActivation = async () => {
      if (disposed || consuming) return;
      consuming = true;
      try {
        const activation = await invoke<unknown>("uapi_take_manager_activation");
        if (!disposed) {
          await handleManagerActivation(
            activation,
            () => setRoute("connection"),
            refreshState,
          );
        }
      } catch {
        // 激活标记只是窗口间提示；一次读取失败不应打断当前页面操作。
      } finally {
        consuming = false;
      }
    };

    void consumePendingActivation();
    const timer = window.setInterval(consumePendingActivation, MANAGER_ACTIVATION_POLL_MS);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [refreshState]);

  const run = useCallback(async <T,>(action: Exclude<BusyAction, null>, task: () => Promise<T>) => {
    interactionGeneration.current += 1;
    setBusy(action);
    setNotice(null);
    try {
      return await task();
    } catch (error) {
      setNotice({ kind: "error", text: friendlyError(error) });
      return null;
    } finally {
      setBusy(null);
    }
  }, []);

  const refreshStatus = useCallback(async () => {
    interactionGeneration.current += 1;
    setBusy("status");
    setNotice(null);
    try {
      await refreshLatestState();
      setNotice({ kind: "ok", text: "状态已刷新。" });
    } catch (error) {
      setNotice({ kind: "error", text: `状态刷新失败：${friendlyError(error)}` });
    } finally {
      setBusy(null);
    }
  }, [refreshLatestState]);

  const refreshAfterMutation = useCallback(async (completedMessage: string) => {
    try {
      await refreshLatestState();
      return true;
    } catch (error) {
      setNotice({
        kind: "error",
        text: `${completedMessage}，但状态刷新失败：${friendlyError(error)}。可点击“刷新状态”重试。`,
      });
      return false;
    }
  }, [refreshLatestState]);

  const validateKey = useCallback(async () => {
    const result = await run("validate", async () =>
      invoke<UapiModelDiscovery>("uapi_validate_key", { request: { apiKey } }),
    );
    if (!result) return;
    if (result.status === "ok") {
      setDiscovery(result);
    }
    setNotice({
      kind: result.status === "ok" ? "ok" : "error",
      text:
        result.status === "ok"
          ? `密钥验证通过，已获取 ${result.compatibleModels.length} 个候选模型；实际可用性以首次对话为准。`
          : result.message,
    });
  }, [apiKey, run]);

  const configure = useCallback(async () => {
    await run("configure", async () => {
      const result = await invoke<UapiApplyResult>("uapi_configure", { request: { apiKey } });
      if (result.status !== "ok" || !result.configured) {
        setNotice({ kind: "error", text: result.message || "配置失败，原配置未修改。" });
        return;
      }
      setApiKey("");
      setDiscovery(null);
      setRestartRequired(true);
      if (!await refreshAfterMutation("配置已保存")) return;
      setNotice({
        kind: "ok",
        text: `配置完成，已同步 ${result.compatibleModels.length} 个模型。请点击“重启 Codex”应用新配置。`,
      });
    });
  }, [apiKey, refreshAfterMutation, run]);

  const refreshModels = useCallback(async () => {
    await run("refresh", async () => {
      const result = await invoke<UapiApplyResult>("uapi_refresh_models");
      if (result.status !== "ok") {
        setNotice({ kind: "error", text: result.message });
        return;
      }
      setRestartRequired(true);
      if (!await refreshAfterMutation("模型目录已更新")) return;
      setNotice({
        kind: "ok",
        text: `模型目录已更新，共 ${result.compatibleModels.length} 个可用于 Codex 的候选模型。请点击“重启 Codex”应用新目录。`,
      });
    });
  }, [refreshAfterMutation, run]);

  const switchMode = useCallback(async (mode: UapiConnectionMode) => {
    await run("switchMode", async () => {
      const result = await invoke<UapiModeSwitchResult>("uapi_switch_mode", { request: { mode } });
      if (result.status !== "ok") {
        setNotice({ kind: "error", text: result.message });
        return;
      }
      setDiscovery(null);
      if (result.restartRequired) {
        setRestartRequired(true);
      }
      let refreshedStatus: UapiStatus;
      try {
        refreshedStatus = await refreshLatestState();
      } catch (error) {
        setNotice({ kind: "error", text: `模式已切换，但状态刷新失败：${friendlyError(error)}` });
        return;
      }
      if (mode === "official" && !result.officialAuthenticated) {
        setNotice({ kind: "info", text: "已切到官方订阅。请点击“重启 Codex”，再按原生流程登录 ChatGPT。" });
        return;
      }
      if (mode === "uapi" && !result.configured && !refreshedStatus.uapiReady) {
        setRoute("connection");
        setNotice({ kind: "info", text: "已切回 U-API Connect，请填写服务密钥完成配置。" });
        return;
      }
      setNotice({
        kind: "ok",
        text: mode === "official" ? "已切到官方订阅，请点击“重启 Codex”应用。" : "已切回 U-API Connect，请点击“重启 Codex”应用。",
      });
    });
  }, [refreshLatestState, run]);

  const launch = useCallback(async () => {
    if (!canLaunch(status)) {
      setRoute("connection");
      setNotice({
        kind: "info",
        text: "请先填写服务密钥。",
      });
      return;
    }
    const command = launchCommand(restartRequired);
    const result = await run("launch", async () =>
      invoke<CommandResult<Record<string, unknown>>>(command, {
        request: {
          appPath: "",
          debugPort: 9229,
          helperPort: 57321,
          syncActiveRelay: false,
        },
      }),
    );
    if (!result) return;
    const accepted = result.status === "accepted" || result.status === "ok";
    if (accepted) {
      setRestartRequired(false);
    }
    setNotice({
      kind: accepted ? "ok" : "error",
      text: result.message,
    });
    const launchInteraction = interactionGeneration.current;
    window.setTimeout(() => {
      void refreshState().catch((error) => {
        if (
          error instanceof StaleRefreshError
          || interactionGeneration.current !== launchInteraction
        ) return;
        setNotice({
          kind: "error",
          text: `启动请求已提交，但状态刷新失败：${friendlyError(error)}。可点击“刷新状态”重试。`,
        });
      });
    }, 1200);
  }, [refreshState, restartRequired, run, status]);

  const repair = useCallback(async () => {
    await run("repair", async () => {
      const result = await invoke<CommandResult<Record<string, unknown>>>("repair_shortcuts");
      if (result.status !== "ok") {
        setNotice({ kind: "error", text: result.message });
        return;
      }
      if (!await refreshAfterMutation("入口已修复")) return;
      setNotice({ kind: "ok", text: result.message });
    });
  }, [refreshAfterMutation, run]);

  const copyDiagnostics = useCallback(async () => {
    const result = await run("diagnostics", async () => invoke<DiagnosticsResult>("uapi_diagnostics"));
    if (!result) return;
    try {
      await navigator.clipboard.writeText(result.report);
      setNotice({ kind: "ok", text: "诊断信息已复制，服务密钥已脱敏。" });
    } catch {
      setNotice({ kind: "error", text: "复制失败，请检查系统剪贴板权限。" });
    }
  }, [run]);

  const activeNav = useMemo(() => navItems.find((item) => item.id === route) ?? navItems[0], [route]);

  return (
    <div className={`shell uapi-shell ${theme}`}>
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">U</div>
          <div className="brand-copy">
            <div className="brand-title-row">
              <strong className="brand-title">{distribution.productName}</strong>
            </div>
            <div className="brand-subtitle">{distribution.productSubtitle}</div>
          </div>
        </div>
        <nav className="nav" aria-label="主导航">
          {navItems.map((item) => {
            const Icon = item.icon;
            return (
              <button
                aria-label={item.label}
                aria-current={route === item.id ? "page" : undefined}
                className={`nav-item ${route === item.id ? "active" : ""}`}
                key={item.id}
                onClick={() => {
                  setRoute(item.id);
                  setNotice(null);
                }}
                title={item.label}
                type="button"
              >
                <span className="nav-icon"><Icon className="h-4 w-4" /></span>
                <span className="uapi-nav-copy">
                  <strong>{item.label}</strong>
                  <small>{item.detail}</small>
                </span>
              </button>
            );
          })}
        </nav>
      </aside>

      <main className="workspace">
        <header className="topbar">
          <div>
            <h1>{activeNav.label}</h1>
            <p>{pageDescription(route)}</p>
          </div>
          <div className="topbar-actions">
            <Button
              aria-label="切换主题"
              onClick={() => setTheme((current) => (current === "dark" ? "light" : "dark"))}
              size="icon"
              variant="ghost"
            >
              {theme === "dark" ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
            </Button>
            <Button disabled={!status || busy !== null} onClick={() => void launch()}>
              <Rocket className="h-4 w-4" />
              {!status
                ? statusLoadFailed ? "状态不可用" : "正在检查…"
                : busy === "launch"
                  ? (restartRequired ? "正在重启…" : "正在启动…")
                  : restartRequired ? "重启 Codex" : "启动 Codex"}
            </Button>
          </div>
        </header>

        <section className="screen uapi-screen">
          {notice ? <NoticeBox notice={notice} /> : null}
          {route === "overview" ? (
            <OverviewScreen
              busy={busy}
              onConfigure={() => setRoute("connection")}
              onLaunch={launch}
              onRefresh={refreshStatus}
              overview={overview}
              restartRequired={restartRequired}
              status={status}
              statusLoadFailed={statusLoadFailed}
            />
          ) : null}
          {route === "connection" ? (
            <ConnectionScreen
              apiKey={apiKey}
              busy={busy}
              discovery={discovery}
              onApiKey={(value) => {
                setApiKey(value);
                setDiscovery(null);
              }}
              onConfigure={configure}
              onRefreshModels={refreshModels}
              onSwitchMode={switchMode}
              onToggleKey={() => setShowKey((current) => !current)}
              onValidate={validateKey}
              showKey={showKey}
              status={status}
            />
          ) : null}
          {route === "maintenance" ? (
            <MaintenanceScreen
              busy={busy}
              onCopyDiagnostics={copyDiagnostics}
              onRefresh={refreshStatus}
              onRefreshModels={refreshModels}
              onRepair={repair}
              overview={overview}
              status={status}
            />
          ) : null}
          {route === "about" ? (
            <AboutScreen
              onOpen={(url) => {
                void invoke("open_external_url", { url }).catch((error) => {
                  setNotice({ kind: "error", text: `打开链接失败：${friendlyError(error)}` });
                });
              }}
              version={version || overview?.current_version || "-"}
            />
          ) : null}
        </section>
      </main>
    </div>
  );
}

function OverviewScreen({
  overview,
  status,
  statusLoadFailed,
  busy,
  restartRequired,
  onLaunch,
  onConfigure,
  onRefresh,
}: {
  overview: OverviewResult | null;
  status: UapiStatus | null;
  statusLoadFailed: boolean;
  busy: BusyAction;
  restartRequired: boolean;
  onLaunch: () => Promise<void>;
  onConfigure: () => void;
  onRefresh: () => Promise<void>;
}) {
  const codexReady = overview?.codex_app.status === "found";
  const usingOfficial = status?.connectionMode === "official";
  const launchAllowed = canLaunch(status);
  const statusLoading = status === null && !statusLoadFailed;
  const statusUnavailable = status === null && statusLoadFailed;
  return (
    <div className="uapi-stack">
      <Card className="panel uapi-hero-panel">
        <CardContent className="uapi-hero-content">
          <div className="uapi-hero-icon"><Network className="h-6 w-6" /></div>
          <div className="uapi-hero-copy">
            <span className="eyebrow">{statusLoading ? "正在检查本地状态" : statusUnavailable ? "本地状态暂不可用" : usingOfficial ? "官方订阅 · 临时模式" : "固定服务 · 动态模型"}</span>
            <h2>{statusLoading
              ? "正在读取 U-API Connect 配置"
              : statusUnavailable
                ? "状态读取失败"
              : usingOfficial
              ? status?.officialAuthenticated ? "Codex 正使用官方订阅" : "Codex 已切到官方订阅模式"
              : "Codex 已接入 U-API Connect"}</h2>
            <p>{statusLoading
              ? "完成检查后即可启动 Codex 或继续配置。"
              : statusUnavailable
                ? "当前没有足够信息判断连接状态，请重新读取后再启动。"
              : usingOfficial
              ? status?.officialAuthenticated
                ? "官方登录已就绪；完成测试后可随时切回默认服务。"
                : "启动 Codex 后可按原生流程登录 ChatGPT；完成测试后可随时切回默认服务。"
              : "中转地址已内置，模型会按当前密钥权限自动同步。"}</p>
          </div>
          <div className="uapi-hero-actions">
            {statusLoading ? (
              <Button disabled><Activity className="h-4 w-4" />正在检查…</Button>
            ) : statusUnavailable ? (
              <Button disabled={busy !== null} onClick={() => void onRefresh()} variant="outline">
                <RefreshCw className="h-4 w-4" />重新读取
              </Button>
            ) : launchAllowed ? (
              <Button disabled={busy !== null} onClick={() => void onLaunch()}>
                <Play className="h-4 w-4" />{restartRequired ? "重启 Codex" : "启动 Codex"}
              </Button>
            ) : (
              <Button onClick={onConfigure}><KeyRound className="h-4 w-4" />立即配置</Button>
            )}
          </div>
        </CardContent>
      </Card>

      <Card className="panel">
        <CardHeader>
          <CardTitle>运行状态</CardTitle>
          <CardDescription>只展示使用时真正需要关注的状态。</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="health-grid uapi-health-grid">
            <HealthItem
              detail={overview?.codex_version ?? "未检测到 Codex Desktop"}
              ok={codexReady}
              title="Codex Desktop"
            />
            <HealthItem
              detail={usingOfficial
                ? status?.officialAuthenticated ? "官方订阅已就绪" : "尚未登录，启动 Codex 后完成登录"
                : status?.configured
                  ? `密钥 ${status.apiKeyMasked || "已保存"}`
                  : status?.uapiReady
                    ? "凭证与模型已保存，启动时会修复连接"
                    : "尚未配置服务密钥"}
              ok={usingOfficial
                ? Boolean(status?.officialAuthenticated)
                : Boolean(status?.configured || status?.uapiReady)}
              title={usingOfficial ? "官方订阅" : "AI 服务"}
            />
            <HealthItem
              detail={usingOfficial ? "由 Codex 官方账号管理" : status?.modelCount ? `${status.modelCount} 个候选模型` : "尚未同步模型"}
              ok={usingOfficial || Boolean(status?.modelCount)}
              title="模型目录"
            />
            <HealthItem
              detail={usingOfficial ? "由 Codex 自动选择" : status?.currentModel || "尚未选择"}
              ok={usingOfficial || Boolean(status?.currentModel)}
              title="当前模型"
            />
          </div>
          <div className="toolbar">
            <Button disabled={busy !== null} onClick={() => void onRefresh()} variant="outline">
              <RefreshCw className="h-4 w-4" />{busy === "status" ? "正在刷新…" : "刷新状态"}
            </Button>
            <Button onClick={onConfigure} variant="outline">
              <Settings2 className="h-4 w-4" />连接设置
            </Button>
          </div>
        </CardContent>
      </Card>

      <Card className="panel">
        <CardHeader>
          <CardTitle>最近启动</CardTitle>
          <CardDescription>{overview?.logs_path ?? "暂无状态文件"}</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="status-table">
            <StatusRow label="状态" value={launchStatusText(overview?.latest_launch?.status)} />
            <StatusRow label="信息" value={overview?.latest_launch?.message || "暂无启动记录"} />
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function ConnectionScreen({
  apiKey,
  showKey,
  status,
  discovery,
  busy,
  onApiKey,
  onToggleKey,
  onValidate,
  onConfigure,
  onRefreshModels,
  onSwitchMode,
}: {
  apiKey: string;
  showKey: boolean;
  status: UapiStatus | null;
  discovery: UapiModelDiscovery | null;
  busy: BusyAction;
  onApiKey: (value: string) => void;
  onToggleKey: () => void;
  onValidate: () => Promise<void>;
  onConfigure: () => Promise<void>;
  onRefreshModels: () => Promise<void>;
  onSwitchMode: (mode: UapiConnectionMode) => Promise<void>;
}) {
  return (
    <div className="uapi-stack">
      <Card className="panel">
        <CardHeader>
          <CardTitle>使用模式</CardTitle>
          <CardDescription>默认使用 U-API Connect；官方订阅仅作为临时开发、测试或自用通道。</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="status-table">
            <StatusRow
              label="当前模式"
              value={status?.connectionMode === "official" ? "官方订阅" : "U-API Connect（默认）"}
            />
            <StatusRow
              label="官方登录"
              value={status?.officialAuthenticated
                ? status.officialAccountLabel || (status.officialLoginSaved ? "已安全保存" : "当前已登录")
                : status?.connectionMode === "official" ? "尚未登录，可启动 Codex 完成登录" : "尚未登录"}
            />
            <StatusRow label="凭证保管" value={status?.credentialStoreMessage || "正在检查系统凭证库"} />
          </div>
          <div className="toolbar">
            {status?.connectionMode === "official" ? (
              <Button
                disabled={!status || busy !== null}
                onClick={() => void onSwitchMode("uapi")}
              >
                <Network className="h-4 w-4" />
                {busy === "switchMode" ? "正在切换…" : "切回 U-API Connect"}
              </Button>
            ) : (
              <Button
                disabled={!status || busy !== null}
                onClick={() => void onSwitchMode("official")}
                variant="outline"
              >
                <ShieldCheck className="h-4 w-4" />
                {busy === "switchMode" ? "正在切换…" : "临时使用官方订阅"}
              </Button>
            )}
          </div>
        </CardContent>
      </Card>

      <Card className="panel">
        <CardHeader>
          <CardTitle>服务连接</CardTitle>
          <CardDescription>服务地址和 Responses 协议已由发行版内置，用户只需填写密钥。</CardDescription>
        </CardHeader>
        <CardContent className="uapi-form">
          <div className="uapi-locked-service">
            <ShieldCheck className="h-5 w-5" />
            <div>
              <strong>固定服务已启用</strong>
              <span>不能在界面切换到其他 Token 供应商</span>
            </div>
          </div>
          <label className="field uapi-key-field">
            <span>服务密钥</span>
            <div className="uapi-key-row">
              <Input
                autoComplete="off"
                disabled={busy !== null}
                onChange={(event: ChangeEvent<HTMLInputElement>) => onApiKey(event.target.value)}
                placeholder={status?.apiKeyMasked ? `当前：${status.apiKeyMasked}` : "粘贴 sk- 开头的密钥"}
                type={showKey ? "text" : "password"}
                value={apiKey}
              />
              <Button aria-label="显示或隐藏密钥" disabled={busy !== null} onClick={onToggleKey} size="icon" type="button" variant="outline">
                {showKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
              </Button>
            </div>
            <small>新密钥验证并成功写入后，才会替换当前有效配置。</small>
          </label>
          <div className="toolbar">
            <Button disabled={!apiKey.trim() || busy !== null} onClick={() => void onValidate()} variant="secondary">
              <Network className="h-4 w-4" />
              {busy === "validate" ? "正在验证…" : "验证密钥"}
            </Button>
            <Button disabled={!apiKey.trim() || busy !== null} onClick={() => void onConfigure()}>
              <KeyRound className="h-4 w-4" />
              {busy === "configure" ? "正在保存…" : "保存配置"}
            </Button>
            <Button disabled={!status?.configured || busy !== null} onClick={() => void onRefreshModels()} variant="outline">
              <RefreshCw className="h-4 w-4" />刷新模型
            </Button>
          </div>
        </CardContent>
      </Card>

      <Card className="panel">
        <CardHeader>
          <CardTitle>模型同步</CardTitle>
          <CardDescription>根据服务返回的接口声明筛选候选模型；实际可用性以首次对话为准。</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="status-table">
            <StatusRow label="当前模型" value={status?.currentModel || "尚未配置"} />
            <StatusRow label="候选模型" value={`${discovery?.compatibleModels.length ?? status?.modelCount ?? 0} 个`} />
            <StatusRow label="未纳入目录" value={`${discovery?.filteredModels.length ?? 0} 个`} />
          </div>
          <ModelList models={discovery?.compatibleModels ?? status?.compatibleModels ?? []} />
        </CardContent>
      </Card>
    </div>
  );
}

function MaintenanceScreen({
  overview,
  status,
  busy,
  onRefresh,
  onRefreshModels,
  onRepair,
  onCopyDiagnostics,
}: {
  overview: OverviewResult | null;
  status: UapiStatus | null;
  busy: BusyAction;
  onRefresh: () => Promise<void>;
  onRefreshModels: () => Promise<void>;
  onRepair: () => Promise<void>;
  onCopyDiagnostics: () => Promise<void>;
}) {
  const usingOfficial = status?.connectionMode === "official";
  const uapiLocallyReady = Boolean(status?.configured || status?.uapiReady);
  return (
    <div className="uapi-stack">
      <Card className="panel">
        <CardHeader>
          <CardTitle>检查结果</CardTitle>
          <CardDescription>技术细节仍由 CodexPlusPlus 底层处理，界面只呈现关键结果。</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="health-grid uapi-health-grid">
            <HealthItem detail={overview?.codex_app.path || "未检测到"} ok={overview?.codex_app.status === "found"} title="Codex 安装" />
            <HealthItem
              detail={usingOfficial
                ? status?.officialAuthenticated ? "官方登录已就绪" : "等待官方登录"
                : status?.configured ? "配置完整" : status?.uapiReady ? "缓存完整，启动时会自动修复" : "需要重新配置"}
              ok={usingOfficial ? Boolean(status?.officialAuthenticated) : uapiLocallyReady}
              title="本地配置"
            />
            <HealthItem detail={usingOfficial ? "官方订阅" : status?.active ? "受管 Provider 已激活" : "未激活"} ok={usingOfficial || Boolean(status?.active)} title="服务路由" />
            <HealthItem detail={usingOfficial ? "官方管理" : status?.modelCount ? `${status.modelCount} 个` : "未生成"} ok={usingOfficial || Boolean(status?.modelCount)} title="模型目录" />
          </div>
          <div className="toolbar">
            <Button disabled={busy !== null} onClick={() => void onRefresh()}>
              <Stethoscope className="h-4 w-4" />{busy === "status" ? "正在检查…" : "一键检查"}
            </Button>
            <Button disabled={busy !== null} onClick={() => void onRepair()} variant="secondary">
              <Wrench className="h-4 w-4" />修复入口
            </Button>
            <Button disabled={usingOfficial || !status?.configured || busy !== null} onClick={() => void onRefreshModels()} variant="secondary">
              <RefreshCw className="h-4 w-4" />重建模型目录
            </Button>
            <Button disabled={busy !== null} onClick={() => void onCopyDiagnostics()} variant="outline">
              <ClipboardCopy className="h-4 w-4" />复制诊断信息
            </Button>
          </div>
        </CardContent>
      </Card>

      <Card className="panel">
        <CardHeader>
          <CardTitle>配置路径</CardTitle>
          <CardDescription>仅用于售后定位，不需要用户手动编辑。</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="status-table">
            <StatusRow label="Codex 配置" value={status?.configPath || "-"} mono />
            <StatusRow label="客户端设置" value={overview?.settings_path || "-"} mono />
            <StatusRow label="诊断日志" value={overview?.logs_path || "-"} mono />
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function AboutScreen({ version, onOpen }: { version: string; onOpen: (url: string) => void }) {
  return (
    <div className="uapi-stack">
      <Card className="panel">
        <CardContent className="uapi-about-hero">
          <div className="brand-mark uapi-about-mark">U</div>
          <div>
            <h2>{distribution.productName}</h2>
            <p>{distribution.productSubtitle}</p>
          </div>
          <span className="uapi-version">v{version}</span>
        </CardContent>
      </Card>
      <Card className="panel">
        <CardHeader>
          <CardTitle>产品信息</CardTitle>
          <CardDescription>基于 CodexPlusPlus 的定制发行版，保留上游许可和源码归属。</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="status-table">
            <StatusRow label="发行主体" value={distribution.publisher} />
            <StatusRow label="服务模式" value="固定 NewAPI · 动态模型目录" />
            <StatusRow label="自动更新" value={distribution.features.updatesEnabled ? "已启用" : "测试版暂未启用"} />
          </div>
          <div className="toolbar">
            <Button onClick={() => onOpen(distribution.helpUrl)} variant="secondary">
              <ExternalLink className="h-4 w-4" />帮助与服务
            </Button>
            <Button onClick={() => onOpen(distribution.upstreamSourceUrl)} variant="outline">
              <ExternalLink className="h-4 w-4" />上游开源项目
            </Button>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function HealthItem({ title, detail, ok }: { title: string; detail: string; ok: boolean }) {
  return (
    <div className={`health-item ${ok ? "ok" : "needs-fix"}`}>
      {ok ? <CheckCircle2 className="h-4 w-4" /> : <CircleAlert className="h-4 w-4" />}
      <div><strong>{title}</strong><span>{detail}</span></div>
      <span className={`uapi-badge ${ok ? "ok" : "warning"}`}>{ok ? "正常" : "待处理"}</span>
    </div>
  );
}

function StatusRow({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="status-row">
      <span>{label}</span>
      {mono ? <code title={value}>{value}</code> : <strong title={value}>{value}</strong>}
    </div>
  );
}

function ModelList({ models }: { models: string[] }) {
  if (!models.length) {
    return <div className="uapi-empty">保存密钥后，将在这里显示可用于 Codex 的候选模型。</div>;
  }
  return (
    <div className="uapi-model-list">
      {models.slice(0, 16).map((model) => <code key={model}>{model}</code>)}
      {models.length > 16 ? <span>另有 {models.length - 16} 个模型</span> : null}
    </div>
  );
}

function NoticeBox({ notice }: { notice: Notice }) {
  const Icon = notice.kind === "ok" ? CheckCircle2 : notice.kind === "error" ? CircleAlert : Info;
  return (
    <div
      aria-atomic="true"
      aria-live={notice.kind === "error" ? "assertive" : "polite"}
      className={`uapi-notice ${notice.kind}`}
      role={notice.kind === "error" ? "alert" : "status"}
    >
      <Icon className="h-4 w-4" />
      <span>{notice.text}</span>
    </div>
  );
}

function pageDescription(route: Route): string {
  switch (route) {
    case "connection": return "填写密钥并同步可用于 Codex 的候选模型";
    case "maintenance": return "检查 Codex、受管 Provider 和模型目录";
    case "about": return "版本、帮助和开源归属";
    default: return "查看 Codex 与 AI 服务的关键状态";
  }
}

function launchStatusText(status?: string | null): string {
  switch (status) {
    case "running": return "运行中";
    case "running_degraded": return "已启动，部分增强等待加载";
    case "starting": return "启动中";
    case "failed": return "启动失败";
    default: return "未启动";
  }
}

function friendlyError(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  try {
    return JSON.stringify(error);
  } catch {
    return "操作失败，请重试。";
  }
}
