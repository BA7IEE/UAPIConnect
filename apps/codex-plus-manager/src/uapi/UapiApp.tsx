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
import { useCallback, useEffect, useMemo, useState, type ChangeEvent } from "react";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { distribution } from "./distribution";
import "./uapi.css";

type Route = "overview" | "connection" | "maintenance" | "about";
type Theme = "dark" | "light";
type BusyAction = "validate" | "configure" | "refresh" | "launch" | "repair" | "diagnostics" | null;

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

export function UapiApp() {
  const [route, setRoute] = useState<Route>(() =>
    new URLSearchParams(window.location.search).get("configure") === "1" ? "connection" : "overview",
  );
  const [theme, setTheme] = useState<Theme>(() =>
    window.localStorage.getItem("codex-plus-theme") === "light" ? "light" : "dark",
  );
  const [overview, setOverview] = useState<OverviewResult | null>(null);
  const [status, setStatus] = useState<UapiStatus | null>(null);
  const [version, setVersion] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [busy, setBusy] = useState<BusyAction>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [discovery, setDiscovery] = useState<UapiModelDiscovery | null>(null);

  useEffect(() => {
    document.documentElement.classList.toggle("light", theme === "light");
    document.documentElement.classList.toggle("dark", theme === "dark");
    window.localStorage.setItem("codex-plus-theme", theme);
  }, [theme]);

  const refreshState = useCallback(async () => {
    const [overviewResult, statusResult, versionResult] = await Promise.all([
      invoke<OverviewResult>("load_overview"),
      invoke<CommandResult<UapiStatus>>("uapi_status"),
      invoke<VersionResult>("backend_version"),
    ]);
    setOverview(overviewResult);
    setStatus(statusResult);
    setVersion(versionResult.version);
    if (!statusResult.configured && route === "overview") {
      setRoute("connection");
    }
  }, [route]);

  useEffect(() => {
    void refreshState().catch((error) => {
      setNotice({ kind: "error", text: friendlyError(error) });
    });
  }, [refreshState]);

  const run = useCallback(async <T,>(action: Exclude<BusyAction, null>, task: () => Promise<T>) => {
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

  const validateKey = useCallback(async () => {
    const result = await run("validate", async () =>
      invoke<UapiModelDiscovery>("uapi_validate_key", { request: { apiKey } }),
    );
    if (!result) return;
    setDiscovery(result);
    setNotice({
      kind: result.status === "ok" ? "ok" : "error",
      text:
        result.status === "ok"
          ? `连接正常，发现 ${result.compatibleModels.length} 个兼容 Codex 的模型。`
          : result.message,
    });
  }, [apiKey, run]);

  const configure = useCallback(async () => {
    const result = await run("configure", async () =>
      invoke<UapiApplyResult>("uapi_configure", { request: { apiKey } }),
    );
    if (!result) return;
    if (result.status !== "ok" || !result.configured) {
      setNotice({ kind: "error", text: result.message || "配置失败，原配置未修改。" });
      return;
    }
    setApiKey("");
    setDiscovery(null);
    setNotice({
      kind: "ok",
      text: `配置完成，已同步 ${result.compatibleModels.length} 个模型。`,
    });
    await refreshState();
  }, [apiKey, refreshState, run]);

  const refreshModels = useCallback(async () => {
    const result = await run("refresh", async () => invoke<UapiApplyResult>("uapi_refresh_models"));
    if (!result) return;
    if (result.status !== "ok") {
      setNotice({ kind: "error", text: result.message });
      return;
    }
    setNotice({
      kind: "ok",
      text: `模型目录已更新，共 ${result.compatibleModels.length} 个兼容模型。`,
    });
    await refreshState();
  }, [refreshState, run]);

  const launch = useCallback(async () => {
    if (!status?.configured) {
      setRoute("connection");
      setNotice({ kind: "info", text: "请先填写服务密钥。" });
      return;
    }
    const result = await run("launch", async () => {
      const refreshed = await invoke<UapiApplyResult>("uapi_refresh_models");
      if (refreshed.status !== "ok") {
        setNotice({ kind: "info", text: `模型刷新失败，将使用上次有效配置：${refreshed.message}` });
      }
      return invoke<CommandResult<Record<string, unknown>>>("launch_codex_plus", {
        request: {
          appPath: "",
          debugPort: 9229,
          helperPort: 57321,
          syncActiveRelay: false,
        },
      });
    });
    if (!result) return;
    setNotice({
      kind: result.status === "accepted" || result.status === "ok" ? "ok" : "error",
      text: result.message,
    });
    window.setTimeout(() => void refreshState(), 1200);
  }, [refreshState, run, status?.configured]);

  const repair = useCallback(async () => {
    const result = await run("repair", async () => invoke<CommandResult<Record<string, unknown>>>("repair_shortcuts"));
    if (!result) return;
    setNotice({ kind: result.status === "ok" ? "ok" : "error", text: result.message });
    await refreshState();
  }, [refreshState, run]);

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
    <div className={`shell ${theme}`}>
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
                className={`nav-item ${route === item.id ? "active" : ""}`}
                key={item.id}
                onClick={() => {
                  setRoute(item.id);
                  setNotice(null);
                }}
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
            <Button disabled={busy !== null} onClick={() => void launch()}>
              <Rocket className="h-4 w-4" />
              {busy === "launch" ? "正在启动…" : "启动 Codex"}
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
              onRefresh={refreshState}
              overview={overview}
              status={status}
            />
          ) : null}
          {route === "connection" ? (
            <ConnectionScreen
              apiKey={apiKey}
              busy={busy}
              discovery={discovery}
              onApiKey={setApiKey}
              onConfigure={configure}
              onRefreshModels={refreshModels}
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
              onRefresh={refreshState}
              onRefreshModels={refreshModels}
              onRepair={repair}
              overview={overview}
              status={status}
            />
          ) : null}
          {route === "about" ? (
            <AboutScreen
              onOpen={(url) => void invoke("open_external_url", { url })}
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
  busy,
  onLaunch,
  onConfigure,
  onRefresh,
}: {
  overview: OverviewResult | null;
  status: UapiStatus | null;
  busy: BusyAction;
  onLaunch: () => Promise<void>;
  onConfigure: () => void;
  onRefresh: () => Promise<void>;
}) {
  const codexReady = overview?.codex_app.status === "found";
  return (
    <div className="uapi-stack">
      <Card className="panel uapi-hero-panel">
        <CardContent className="uapi-hero-content">
          <div className="uapi-hero-icon"><Network className="h-6 w-6" /></div>
          <div className="uapi-hero-copy">
            <span className="eyebrow">固定服务 · 动态模型</span>
            <h2>Codex 已接入 U-API Connect</h2>
            <p>中转地址已内置，模型会按当前密钥权限自动同步，并过滤不兼容 Responses API 的模型。</p>
          </div>
          <div className="uapi-hero-actions">
            {status?.configured ? (
              <Button disabled={busy !== null} onClick={() => void onLaunch()}>
                <Play className="h-4 w-4" />启动 Codex
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
              detail={status?.configured ? `密钥 ${status.apiKeyMasked || "已保存"}` : "尚未配置服务密钥"}
              ok={Boolean(status?.configured)}
              title="AI 服务"
            />
            <HealthItem
              detail={status?.modelCount ? `${status.modelCount} 个兼容模型` : "尚未同步模型"}
              ok={Boolean(status?.modelCount)}
              title="模型目录"
            />
            <HealthItem
              detail={status?.currentModel || "尚未选择"}
              ok={Boolean(status?.currentModel)}
              title="当前模型"
            />
          </div>
          <div className="toolbar">
            <Button onClick={() => void onRefresh()} variant="secondary">
              <RefreshCw className="h-4 w-4" />刷新状态
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
}) {
  return (
    <div className="uapi-stack">
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
                onChange={(event: ChangeEvent<HTMLInputElement>) => onApiKey(event.target.value)}
                placeholder={status?.apiKeyMasked ? `当前：${status.apiKeyMasked}` : "粘贴 sk- 开头的密钥"}
                type={showKey ? "text" : "password"}
                value={apiKey}
              />
              <Button aria-label="显示或隐藏密钥" onClick={onToggleKey} size="icon" type="button" variant="outline">
                {showKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
              </Button>
            </div>
            <small>新密钥验证并成功写入后，才会替换当前有效配置。</small>
          </label>
          <div className="toolbar">
            <Button disabled={!apiKey.trim() || busy !== null} onClick={() => void onValidate()} variant="secondary">
              <Network className="h-4 w-4" />
              {busy === "validate" ? "正在验证…" : "测试连接"}
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
          <CardDescription>只同步明确支持 Responses API 的文本模型。</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="status-table">
            <StatusRow label="当前模型" value={status?.currentModel || "尚未配置"} />
            <StatusRow label="可用模型" value={`${discovery?.compatibleModels.length ?? status?.modelCount ?? 0} 个`} />
            <StatusRow label="已过滤" value={`${discovery?.filteredModels.length ?? 0} 个不兼容模型`} />
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
            <HealthItem detail={status?.configured ? "配置完整" : "需要重新配置"} ok={Boolean(status?.configured)} title="本地配置" />
            <HealthItem detail={status?.active ? "受管 Provider 已激活" : "未激活"} ok={Boolean(status?.active)} title="服务路由" />
            <HealthItem detail={status?.modelCount ? `${status.modelCount} 个` : "未生成"} ok={Boolean(status?.modelCount)} title="模型目录" />
          </div>
          <div className="toolbar">
            <Button disabled={busy !== null} onClick={() => void onRefresh()}>
              <Stethoscope className="h-4 w-4" />一键检查
            </Button>
            <Button disabled={busy !== null} onClick={() => void onRepair()} variant="secondary">
              <Wrench className="h-4 w-4" />修复入口
            </Button>
            <Button disabled={!status?.configured || busy !== null} onClick={() => void onRefreshModels()} variant="secondary">
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
            <StatusRow label="服务模式" value="固定 NewAPI · 动态 Responses 模型" />
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
    return <div className="uapi-empty">保存密钥后，将在这里显示可用于 Codex 的模型。</div>;
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
  return <div className={`uapi-notice ${notice.kind}`}><Icon className="h-4 w-4" /><span>{notice.text}</span></div>;
}

function pageDescription(route: Route): string {
  switch (route) {
    case "connection": return "填写密钥并同步兼容 Codex 的动态模型";
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
