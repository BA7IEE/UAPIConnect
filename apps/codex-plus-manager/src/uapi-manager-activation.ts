export const MANAGER_ACTIVATION_POLL_MS = 400;

export async function handleManagerActivation(
  activation: unknown,
  enterConnection: () => void,
  refreshState: () => Promise<unknown>,
): Promise<boolean> {
  if (activation !== "configure") return false;

  enterConnection();
  await refreshState();
  return true;
}
