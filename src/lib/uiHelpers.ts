/**
 * Get button label based on busy state.
 */
export function getBusyLabel(busy: boolean, idleLabel: string, busyLabel: string): string {
  return busy ? busyLabel : idleLabel;
}

/**
 * Get button label for import operations with specific busy type.
 */
export function getImportLabel<T extends string>(
  busyType: T | null,
  targetType: T,
  idleLabel: string,
  busyLabel: string,
): string {
  return busyType === targetType ? busyLabel : idleLabel;
}

/**
 * Check if any value is truthy (for boolean coercion in disabled props).
 */
export function isTruthy(value: unknown): boolean {
  return Boolean(value);
}
