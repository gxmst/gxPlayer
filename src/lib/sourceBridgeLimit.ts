export const MAX_SOURCE_BRIDGE_CALLS = 32;
export const SOURCE_BRIDGE_LIMIT_ERROR = `LX source bridge allows at most ${MAX_SOURCE_BRIDGE_CALLS} concurrent calls`;

export function hasSourceBridgeCapacity(inFlight: number): boolean {
  return Number.isSafeInteger(inFlight) && inFlight >= 0 && inFlight < MAX_SOURCE_BRIDGE_CALLS;
}
