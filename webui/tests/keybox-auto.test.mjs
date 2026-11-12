import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import vm from 'node:vm'
import ts from 'typescript'

const source = readFileSync(new URL('../src/cli.ts', import.meta.url), 'utf8')
const ast = ts.createSourceFile('cli.ts', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
const cli = ast.statements.find(node => ts.isClassDeclaration(node) && node.name.text === 'Cli')
const methods = cli.members.filter(node => ['getKeyboxAuto', 'updateKeyboxAuto'].includes(node.name?.getText(ast)))
const helpers = ast.statements.filter(node => ts.isFunctionDeclaration(node)
  && ['parseKeyboxAuto', 'encodeBase64Bytes', 'encodeBase64Utf8', 'isRecord', 'hasOnlyKeys'].includes(node.name?.text))

function harness() {
  const calls = []
  const defaults = { enabled: false, source: 'yurikey', interval_minutes: 60, proxy_prefix: '' }
  const code = `${helpers.map(node => node.getText(ast)).join('\n')}
    class TestCli {
      async #getHelperPaths() { return { keymint: '/module/keymint' } }
      async #run(command, args) {
        calls.push({ command, args })
        return args.length === 1 ? JSON.stringify(defaults) : atob(args[1])
      }
      ${methods.map(node => node.getText(ast)).join('\n')}
    }
    new TestCli()`
  const client = vm.runInNewContext(ts.transpileModule(code, {
    compilerOptions: { target: ts.ScriptTarget.ES2022 },
  }).outputText, { calls, defaults, URL, TextEncoder, btoa, atob, parseCanonicalJson: JSON.parse })
  return { client, calls, defaults }
}

test('automatic Keybox bridge separates saving and fetching with explicit replacement intent', async () => {
  const { client, calls, defaults } = harness()
  assert.equal((await client.getKeyboxAuto()).interval_minutes, 60)
  assert.deepEqual([...calls[0].args], ['--webui-get-keybox-auto'])
  const settings = { ...defaults, source: 'integrity_box', interval_minutes: 90,
    proxy_prefix: 'https://proxy.example/path/' }
  await client.updateKeyboxAuto(settings)
  await client.updateKeyboxAuto(settings, true)
  for (const [index, command] of [[1, '--webui-save-keybox-auto'], [2, '--webui-fetch-keybox-auto']]) {
    assert.equal(calls[index].args[0], command)
    assert.deepEqual(JSON.parse(atob(calls[index].args[1])), settings)
    assert.equal(calls[index].args[2], '--confirm-replace')
    assert.equal(calls[index].args.length, 3)
  }
})

test('invalid frequency, removed sources and malformed proxies cannot invoke the native helper', async () => {
  const { client, calls, defaults } = harness()
  for (const interval_minutes of [0, -1, 1.5, 43201, '60', NaN]) {
    await assert.rejects(client.updateKeyboxAuto({ ...defaults, interval_minutes }))
  }
  for (const source of ['tricky_addon', 'unknown', '']) {
    await assert.rejects(client.updateKeyboxAuto({ ...defaults, source }))
  }
  for (const proxy_prefix of ['http://proxy.test/', 'https://proxy.test',
    'https://u:p@proxy.test/', 'https://proxy.test/?x=/', 'https://proxy.test/#/',
    ' https://proxy.test/', 'https://proxy.test/' + 'x'.repeat(2048) + '/']) {
    await assert.rejects(client.updateKeyboxAuto({ ...defaults, proxy_prefix }))
  }
  await assert.rejects(client.updateKeyboxAuto({ ...defaults, extra: 'unrecognized' }))
  assert.equal(calls.length, 0)
})

test('back while fetching restores sheet history after the current popstate completes', () => {
  const historySource = readFileSync(new URL('../src/history.ts', import.meta.url), 'utf8')
  const appSource = readFileSync(new URL('../src/vue/App.vue', import.meta.url), 'utf8')
    .split('<script setup lang="ts">')[1].split('</script>')[0]
  const appAst = ts.createSourceFile('App.ts', appSource, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
  const watcher = appAst.statements.find(node => ts.isExpressionStatement(node)
    && ts.isCallExpression(node.expression) && node.expression.expression.getText(appAst) === 'watch'
    && node.expression.arguments[0]?.getText(appAst) === 'keyboxAutoOpen')
  let onPopState
  const ticks = []
  const fakeWindow = {
    history: { replaceState() {}, pushState() {}, back() {} },
    addEventListener(name, callback) { if (name === 'popstate') onPopState = callback },
  }
  const keyboxAutoOpen = { value: true }
  let busy = true
  const globals = { window: fakeWindow, exports: {}, keyboxAutoOpen,
    keyboxAutoDialog: { value: { requestClose() {
      if (busy) return false
      keyboxAutoOpen.value = false
      return true
    } } }, overlayHistory: new Set(), nextTick: callback => ticks.push(callback),
    watch: (_ref, callback) => callback(true),
  }
  const context = vm.createContext(globals)
  const compile = code => ts.transpileModule(code, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  }).outputText
  vm.runInContext(compile(historySource), context)
  const history = new context.exports.History()
  context.history = history
  vm.runInContext(compile(watcher.getText(appAst)), context)
  assert.equal(history.size, 1)
  onPopState({ state: null })
  assert.equal(keyboxAutoOpen.value, true)
  assert.equal(history.size, 0)
  assert.equal(ticks.length, 1)
  ticks.shift()()
  assert.equal(history.size, 1)
  busy = false
  onPopState({ state: null })
  assert.equal(keyboxAutoOpen.value, false)
  assert.equal(history.size, 0)
})
