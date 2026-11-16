// The models a form offers for an agent, from what the hub says it takes (`models`, its default first as ""): no natives,
// so tests load it as it is.

/** The most models drawn as pills: a longer list is typed, with a few of them named as examples. */
export const PILLS = 8;

/**
 * The [value, label] choices for an agent's models, `labels` naming some ("lightest" → "Lightest"), the model already
 * set kept when the agent no longer lists it; none when there is nothing to pick but its default.
 */
export function modelChoices(models, current = "", labels = []) {
  const named = new Map(labels);
  const list = [...(models ?? [])];
  if (!list.includes("")) list.unshift("");
  if (current && !list.includes(current)) list.push(current);
  const out = list.map((m) => [m, named.get(m) ?? (m || "Default")]);
  return out.length > 1 ? out : [];
}

/** Whether the choices are few enough to draw as pills. */
export function asPills(choices) {
  return choices.length > 1 && choices.length <= PILLS;
}

/** A model field's hint for a long list: how many there are, and the first few. */
export function listHint(models) {
  const named = (models ?? []).filter(Boolean);
  if (!named.length) return "";
  const some = named.slice(0, 3).join(", ");
  return `${named.length} models, such as ${some}`;
}
