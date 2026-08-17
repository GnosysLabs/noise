export function groupToHydrate<T extends { group_id: string; is_active: boolean }>(
  groups: readonly T[],
  desiredGroupId: string | null,
): T | undefined {
  if (desiredGroupId) {
    const desired = groups.find((group) => group.group_id === desiredGroupId);
    if (desired) return desired;
  }
  return groups.find((group) => group.is_active);
}
