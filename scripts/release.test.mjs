import assert from "node:assert/strict";
import test from "node:test";
import { analyzeCommits } from "@semantic-release/commit-analyzer";
import { generateNotes } from "@semantic-release/release-notes-generator";
import { spawnSync } from "node:child_process";
import config from "../release.config.mjs";

const logger = { log() {} };
const pluginOptions = (name) => config.plugins.find(([plugin]) => plugin === name)[1];
for (const [message, expected] of [
  ["fix: correct status refresh", "patch"],
  ["feat: add notifications", "minor"],
  ["feat!: change configuration format", "major"],
  ["fix: change config\n\nBREAKING CHANGE: old config is unsupported", "major"],
  ["docs: clarify installation", null],
  ["chore: update tooling", null],
]) {
  test(`release analysis: ${message.split("\n")[0]}`, async () => {
    assert.equal(await analyzeCommits(pluginOptions("@semantic-release/commit-analyzer"), {
      commits: [{ message }], logger,
    }), expected);
  });
}

test("release notes include features and breaking changes", async () => {
  const notes = await generateNotes(pluginOptions("@semantic-release/release-notes-generator"), {
    options: { repositoryUrl: "https://github.com/example/OwlGood" },
    commits: [{ hash: "a".repeat(40), message: "feat!: new config\n\nBREAKING CHANGE: migrate config" }],
    lastRelease: { gitTag: "1.0.0" },
    nextRelease: { gitTag: "2.0.0", version: "2.0.0" },
    logger,
  });
  assert.match(notes, /2\.0\.0/);
  assert.match(notes, /new config/);
  assert.match(notes, /migrate config/);
});

test("invalid release versions are rejected before building", () => {
  for (const version of ["v1.2.3", "01.2.3", "1.2.3-beta.1", "invalid"]) {
    const result = spawnSync("bash", ["scripts/prepare-release.sh", version], { encoding: "utf8" });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /Invalid release version/);
  }
});
