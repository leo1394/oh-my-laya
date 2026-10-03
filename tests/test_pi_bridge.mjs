import assert from "node:assert/strict";
import extension from "../integrations/pi/extensions/oh-my-laya.ts";

const tools = new Map();
const events = new Map();
globalThis.layaCalls = [];
globalThis.layaResult = { content: [{type: "text", text: "raw advice"}], structuredContent: {ok: true} };
extension({registerTool: tool => tools.set(tool.name, tool), on: (name, handler) => events.set(name, handler)});
assert.equal(tools.size, 3);
const advisor = {models: [{id: "verified", reasoning_efforts: ["low"]}], current_model: "verified"};
assert.equal((await tools.get("laya_tell_me").execute("call", {state: "task", advisor})).content[0].text, "raw advice");
assert.deepEqual(globalThis.layaCalls[0], {name: "laya_tell_me", arguments: {state: "task", advisor}});
await tools.get("laya_advisor_preferences").execute("call", {});
assert.deepEqual(globalThis.layaCalls[1], {name: "laya_advisor_preferences", arguments: {}});
const preferences = {policy: "auto", ceiling: {model: "verified", reasoning_effort: "low"}, models: advisor.models};
await tools.get("laya_advisor_preferences").execute("call", preferences);
assert.deepEqual(globalThis.layaCalls[2].arguments, preferences);
const feedback = {
  protocol_version: 1, event_id: "event-1", decision_id: "decision-1",
  attempt_ref: "attempt-1", kind: "test",
  source: {host: "pi", role: "tester", actor_type: "agent"},
  payload: {outcome: "passed"},
};
await tools.get("laya_feedback").execute("call", feedback);
assert.deepEqual(globalThis.layaCalls[3], {name: "laya_feedback", arguments: feedback});
globalThis.layaResult = {content: [{type: "text", text: "bad model"}], isError: true};
await assert.rejects(tools.get("laya_tell_me").execute("call", {state: "task", advisor}), /bad model/);
await events.get("session_shutdown")();
assert.equal(globalThis.layaClosed, true);
console.log("PASS: pi advisor forwarding, preferences read/write, errors and shutdown (mock MCP)");
