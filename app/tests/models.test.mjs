import assert from "node:assert/strict";
import { test } from "node:test";

import { modelChoices, asPills, listHint, PILLS } from "../mods/cockpit/models.js";

const CLAUDE = [["", "Default"], ["lightest", "Lightest"], ["haiku", "Haiku"], ["sonnet", "Sonnet"], ["opus", "Opus"]];

test("a routine's form offers the models its agent takes, its default first", () => {
  assert.deepEqual(modelChoices(["", "lightest", "haiku"], "", CLAUDE), [["", "Default"], ["lightest", "Lightest"], ["haiku", "Haiku"]], "Claude Code's, by their labels");
  assert.deepEqual(modelChoices(["acme/large", "acme/small"], "acme/small"), [["", "Default"], ["acme/large", "acme/large"], ["acme/small", "acme/small"]], "another agent's as it lists them");
  assert.deepEqual(modelChoices([], "acme/old"), [["", "Default"], ["acme/old", "acme/old"]], "a model set before stays offered");
  assert.deepEqual(modelChoices([""], ""), [], "nothing to pick but its default: no model row");
  assert.deepEqual(modelChoices(undefined, ""), []);
});

test("a long list is typed, with how many there are and a few names", () => {
  const many = Array.from({ length: 40 }, (_, i) => `acme/model-${i}`);
  assert.equal(asPills(modelChoices(many)), false);
  assert.equal(asPills(modelChoices(many.slice(0, PILLS - 1))), true);
  assert.equal(listHint(["", ...many]), "40 models, such as acme/model-0, acme/model-1, acme/model-2");
  assert.equal(listHint([""]), "");
});
