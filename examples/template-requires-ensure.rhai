// Every template must declare an ensure clause.
// Regex can't express "template node missing a child" — this is AST-only.

const NAME = "template-requires-ensure";
const SEVERITY = "medium";
const DESCRIPTION = "Every template must declare an ensure clause";

fn on_template(template) {
    if template.ensure_clause == () {
        report(template, `Template '${template.name}' has no ensure clause`);
    }
}
