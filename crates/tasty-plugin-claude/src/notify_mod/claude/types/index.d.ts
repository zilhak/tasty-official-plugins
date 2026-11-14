export type NotifyPending = {
  namespace: string;
  seq: number;
  text: string;
  next: number;
  retention: number;
};
export type NotifyCursor = {
  namespace: string;
  next: number;
  retention: number;
  missing: boolean;
  seq?: number;
  pending?: NotifyPending;
};
declare module 'claude-code' {
  interface PluginState {
    'tasty-notify': { cursor: NotifyCursor };
  }
}
