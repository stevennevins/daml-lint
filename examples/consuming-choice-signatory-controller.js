// Compiled from consuming-choice-signatory-controller.ts — this is the file you pass to --rules.

const NAME = "consuming-choice-signatory-controller";
const SEVERITY = "medium";
const DESCRIPTION = "Consuming choices should have at least one signatory controller";

function on_choice(choice, template) {
  if (!choice.consuming) {
    return;
  }
  // `controller signatory this` is signatory-controlled by definition.
  if (choice.controllers.some((c) => c.startsWith("signatory") || template.signatories.includes(c))) {
    return;
  }
  report(choice, `Consuming choice '${choice.name}' has no signatory among its controllers`);
}
