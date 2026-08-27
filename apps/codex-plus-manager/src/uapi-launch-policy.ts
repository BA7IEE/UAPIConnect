export type UapiLaunchStatus = {
  connectionMode: "uapi" | "official";
  configured: boolean;
  uapiReady: boolean;
  officialAuthenticated: boolean;
  credentialStoreAvailable?: boolean;
};

export type UapiLaunchCommand = "launch_codex_plus" | "restart_codex_plus";

export function canLaunch(status: UapiLaunchStatus | null | undefined): boolean {
  if (!status) return false;
  // 官方登录发生在原生 Codex 内，不能把登录状态作为启动前置条件。
  // 已有安全凭证和模型档案时，launcher 会在启动过程中修复缺失或被改动的 live 配置。
  return status.connectionMode === "official" || status.configured || status.uapiReady;
}

export function launchCommand(restartRequired: boolean): UapiLaunchCommand {
  return restartRequired ? "restart_codex_plus" : "launch_codex_plus";
}
