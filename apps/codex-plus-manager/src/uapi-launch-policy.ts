export type UapiLaunchStatus = {
  connectionMode: "uapi" | "official";
  configured: boolean;
  officialAuthenticated: boolean;
  credentialStoreAvailable?: boolean;
};

export function canLaunch(status: UapiLaunchStatus | null | undefined): boolean {
  if (!status) return false;
  // 官方登录发生在原生 Codex 内，不能把登录状态作为启动前置条件。
  return status.connectionMode === "official" || status.configured;
}
