// Consuming choices should have at least one controller who is a signatory.
// Needs cross-referencing two AST node lists (controllers vs signatories).

const NAME = "consuming-choice-signatory-controller";
const SEVERITY = "medium";
const DESCRIPTION = "Consuming choices should have at least one signatory controller";

fn on_choice(choice, template) {
    if !choice.consuming {
        return;
    }
    for controller in choice.controllers {
        if controller in template.signatories {
            return;
        }
    }
    report(choice, `Consuming choice '${choice.name}' has no signatory among its controllers`);
}
