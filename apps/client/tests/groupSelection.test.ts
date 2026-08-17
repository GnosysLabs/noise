import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { groupToHydrate } from "../src/groupSelection.ts";

function group(group_id: string, is_active = false) {
  return { group_id, is_active };
}

describe("groupToHydrate", () => {
  it("keeps the group the UI is already showing", () => {
    const groups = [group("top"), group("joined", true)];
    assert.equal(groupToHydrate(groups, "top")?.group_id, "top");
  });

  it("falls back to the backend-active group before anything is selected", () => {
    const groups = [group("top"), group("joined", true)];
    assert.equal(groupToHydrate(groups, null)?.group_id, "joined");
  });

  it("falls back when the desired group is no longer in the list", () => {
    const groups = [group("joined", true)];
    assert.equal(groupToHydrate(groups, "left")?.group_id, "joined");
  });

  it("returns nothing when there is no group to show", () => {
    assert.equal(groupToHydrate([], "top"), undefined);
    assert.equal(groupToHydrate([group("top")], null), undefined);
  });
});
