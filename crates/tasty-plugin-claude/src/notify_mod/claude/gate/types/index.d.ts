declare module 'claude-code' {
  interface PluginState {
    'tasty-notify-gate': { record: GateRecord };
  }
}
/**
 * `delivered`: the highest sequence that entered. `inflight`: a sequence whose
 * entry is still running in the gate copy named by `owner` (0 and '' when none).
 */
export type GateRecord = { delivered: number; inflight: number; owner: string };
