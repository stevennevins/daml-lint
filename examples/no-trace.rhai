// Flag leftover debug trace calls — a simple banned-token rule written
// as a whole-module check over the raw source.

const NAME = "no-trace";
const SEVERITY = "low";
const DESCRIPTION = "Debug trace left in code";

fn check(m) {
    let line_no = 1;
    for line in m.source.split("\n") {
        if line.contains("trace") {
            report(line_no, "Remove debug trace calls before deploying");
        }
        line_no += 1;
    }
}
