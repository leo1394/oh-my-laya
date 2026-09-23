import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { Type } from "@sinclair/typebox";

type Config = {
  command: string;
  args: string[];
  modelDir: string;
};

const configUrl = new URL("../laya.config.json", import.meta.url);
const config = JSON.parse(readFileSync(fileURLToPath(configUrl), "utf8")) as Config;

export default function (pi: ExtensionAPI) {
  let client: Client | undefined;
  let connecting: Promise<Client> | undefined;

  async function getClient(): Promise<Client> {
    if (client) return client;
    if (connecting) return connecting;

    connecting = (async () => {
      const nextClient = new Client({ name: "pi-oh-my-laya", version: "0.1.0" });
      const transport = new StdioClientTransport({
        command: config.command,
        args: config.args,
        env: {
          ...process.env,
          LAYA_MODEL_DIR: config.modelDir,
        } as Record<string, string>,
        stderr: "inherit",
      });
      await nextClient.connect(transport);
      client = nextClient;
      return nextClient;
    })();

    try {
      return await connecting;
    } finally {
      connecting = undefined;
    }
  }

  pi.registerTool({
    name: "laya_tell_me",
    label: "Laya Tell Me",
    description:
      "Run a fast local Laya-MLX choice, score, or noul decision. " +
      "Use for bounded classification and risk triage, not text generation or authorization.",
    parameters: Type.Object({
      state: Type.Any({ description: "Compact text, JSON object, or conversation state" }),
      questions: Type.Record(Type.String(), Type.Any(), {
        description: "Question definitions keyed by question id",
      }),
    }),
    async execute(_toolCallId, params) {
      const connected = await getClient();
      const result = await connected.callTool({
        name: "laya_tell_me",
        arguments: params,
      });

      const content = Array.isArray(result.content) ? result.content : [];
      const text = content
        .filter(
          (item): item is { type: "text"; text: string } =>
            typeof item === "object" &&
            item !== null &&
            "type" in item &&
            item.type === "text" &&
            "text" in item &&
            typeof item.text === "string",
        )
        .map((item) => item.text)
        .join("\n");

      return {
        content: [{ type: "text", text: text || JSON.stringify(result.structuredContent) }],
        details: { structuredContent: result.structuredContent },
      };
    },
  });

  pi.on("session_shutdown", async () => {
    await client?.close();
    client = undefined;
  });
}
