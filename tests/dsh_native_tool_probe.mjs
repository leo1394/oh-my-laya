// Read-only native ToolRuntime discovery; never executes a tool or model request.
export const name = 'laya-native-tool-probe'
export const inject = ['tools']
export function apply(ctx) {
  const timer = setInterval(() => {
    const tools = ctx.tools.schemas().filter(tool => tool.name.startsWith('mcp__oh-my-laya__'))
    if (tools.length < 3) return
    clearInterval(timer)
    console.log('LAYA_NATIVE_TOOLS ' + JSON.stringify(tools.map(tool => tool.name).sort()))
  }, 100)
  ctx.on('dispose', () => clearInterval(timer))
}
