// The gate plugin's contract, for type checking only; that plugin declares it.
declare module 'claude-code' {
  interface PluginState {
    'tasty-notify-gate': { record: { delivered: number; inflight: number; owner: string } };
  }
}
export {};
