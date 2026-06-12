// A choice parameter named identically to a template field silently shadows
// it inside the choice body — `amount` may not be the amount you think.
// Exercises choice.parameters cross-referenced against template.fields.
// Compile: npx esbuild choice-param-shadows-field.ts --outfile=choice-param-shadows-field.js

const NAME = "choice-param-shadows-field";
const SEVERITY = "medium";
const DESCRIPTION = "Choice parameters must not shadow template field names";

function on_choice(choice: Choice, template: Template): void {
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
