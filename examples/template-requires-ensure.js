// Compiled from template-requires-ensure.ts — this is the file you pass to --rules.

const NAME = "template-requires-ensure";
const SEVERITY = "medium";
const DESCRIPTION = "Every template must declare an ensure clause";

function on_template(template) {
  if (template.ensure_clause === null) {
    report(template, `Template '${template.name}' has no ensure clause`);
  }
}
