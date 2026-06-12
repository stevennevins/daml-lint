// Compiled from choice-param-shadows-field.ts — this is the file you pass to --rules.

const NAME = "choice-param-shadows-field";
const SEVERITY = "medium";
const DESCRIPTION = "Choice parameters must not shadow template field names";

function on_choice(choice, template) {
  const fieldNames = new Set(template.fields.map((f) => f.name));
  for (const param of choice.parameters) {
    if (fieldNames.has(param.name)) {
      report(
        param,
        `Parameter '${param.name}' of choice '${choice.name}' shadows a field of template '${template.name}'`
      );
    }
  }
}
