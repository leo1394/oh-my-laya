export async function resolve(specifier, context, next) {
  if (!context.parentURL?.endsWith("/oh-my-laya.ts")) return next(specifier, context);
  const mocks = {
    "node:fs": 'export const readFileSync = () => JSON.stringify({command:"mock",args:[],modelDir:"/mock"});',
    "@modelcontextprotocol/sdk/client/index.js": `export class Client {
      async connect() {}
      async close() { globalThis.layaClosed = true; }
      async callTool(args) { globalThis.layaCalls.push(args); return globalThis.layaResult; }
    }`,
    "@modelcontextprotocol/sdk/client/stdio.js": 'export class StdioClientTransport { constructor() {} }',
    "@sinclair/typebox": 'export const Type = new Proxy({}, {get: (_, type) => (...args) => ({type,args})});',
  };
  if (specifier in mocks) return { url: "data:text/javascript," + encodeURIComponent(mocks[specifier]), shortCircuit: true };
  return next(specifier, context);
}
