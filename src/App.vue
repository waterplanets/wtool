<script setup lang="ts">
import { ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

/** 与 Rust 侧 model::TsSentence 一一对应 */
interface TsSentence {
  source: string;
  target: string;
}

/** 与 Rust 侧 engine::inject::MethodStatus 一一对应 */
interface MethodStatus {
  id: string;
  name: string;
  state: "not_applicable" | "ready" | "installed" | "stale";
  detail: string;
  reversible:
    | "runtime_full"
    | "runtime_and_offline"
    | "offline_only"
    | "none";
  restart_required: boolean;
}

/** 状态/撤销程度的中文说法，只影响显示，逻辑判断还是用原值 */
const STATE_TEXT: Record<MethodStatus["state"], string> = {
  not_applicable: "不适用",
  ready: "未安装",
  installed: "已安装",
  stale: "需重装",
};
const REVERSIBLE_TEXT: Record<MethodStatus["reversible"], string> = {
  runtime_full: "断开即恢复",
  runtime_and_offline: "断开并还原文件",
  offline_only: "只能还原文件",
  none: "只能重启游戏",
};

/** GUI 上填的变量，对应 Rust 侧 task::Config */
const gameDir = ref(
  "D:\\myProgram\\game2\\lilisproject\\SUCCUBUS DESIRE ~リリスプロジェクト~",
);
const dictPath = ref(`${gameDir.value}\\TrsData.bin`);

/** 词典路径是否被手动改过。改过就不再跟着游戏目录自动改，免得覆盖用户的输入。 */
const dictManual = ref(false);

/** 游戏目录改了就顺手把词典路径带过去（MTool 的词典就放在游戏根目录）。 */
watch(gameDir, (dir) => {
  if (!dictManual.value) dictPath.value = `${dir}\\TrsData.bin`;
});

/** 识别出来的引擎，光看这一行就能判断路径填得对不对 */
const engineName = ref("");

const entries = ref<TsSentence[]>([]);
const logs = ref<string[]>([]);
const error = ref("");

/** 这个游戏目录上支持的注入方式，以及各自现在什么状态 */
const methods = ref<MethodStatus[]>([]);

// Rust 那边 app.emit("trans-status", msg)，这里收
listen<string>("trans-status", (e) => {
  logs.value.push(e.payload);
  if (logs.value.length > 200) logs.value.shift();
});

/** 重新探测注入方式。目录一变就重探，免得对着 A 游戏的状态操作 B 游戏。 */
async function refreshInjections() {
  try {
    engineName.value = await invoke<string>("detect_engine", {
      gameDir: gameDir.value,
    });
  } catch {
    engineName.value = "";
  }
  try {
    methods.value = await invoke<MethodStatus[]>("list_injections", {
      gameDir: gameDir.value,
    });
  } catch (e: any) {
    methods.value = [];
    error.value = e?.message ?? String(e);
  }
}

/** 从文件管理器挑游戏目录。目录判据在后端，选中直接填进去就行。 */
async function pickGameDir() {
  const dir = await open({ directory: true, multiple: false });
  if (typeof dir === "string") gameDir.value = dir;
}

/** 从文件管理器挑词典文件，挑完就不跟着游戏目录自动改了。 */
async function pickDict() {
  const file = await open({
    multiple: false,
    filters: [{ name: "MTool 词典", extensions: ["bin", "json"] }],
  });
  if (typeof file === "string") {
    dictPath.value = file;
    dictManual.value = true;
  }
}

// 目录变了重探。immediate 让打开界面就先探一次，用户不用先点一下。
watch(gameDir, refreshInjections, { immediate: true });

/** 「未安装」和「需重装」都能装：后者就是覆盖重装一遍。 */
function canInstall(m: MethodStatus) {
  return m.state === "ready" || m.state === "stale";
}

/** 只有盘上真有 wtool 的东西才谈得上撤销。 */
function canUninstall(m: MethodStatus) {
  return m.state === "installed" || m.state === "stale";
}

/** 注入：把桥写进游戏目录。装完游戏要重启才生效，所以不在这儿自动开任务。 */
async function installMethod(id: string) {
  error.value = "";
  try {
    const lines = await invoke<string[]>("install_injection", {
      gameDir: gameDir.value,
      id,
    });
    logs.value.push(...lines);
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  }
  await refreshInjections();
}

/** 取消注入：先停任务再还原文件 —— 任务还占着旧桥时去删文件容易打架。 */
async function uninstallMethod(id: string) {
  error.value = "";
  try {
    await invoke("stop_trans");
    const lines = await invoke<string[]>("uninstall_injection", {
      gameDir: gameDir.value,
      id,
    });
    logs.value.push(...lines);
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  }
  await refreshInjections();
}

/** 点「开始翻译」：先把变量交给 Rust，再起任务 */
async function start() {
  error.value = "";
  logs.value = [];
  try {
    await invoke("set_config", {
      gameDir: gameDir.value,
      dictPath: dictPath.value,
    });
    await invoke("start_trans");
  } catch (e: any) {
    error.value = e?.message ?? String(e);
  }
}

async function stop() {
  await invoke("stop_trans");
}

async function load() {
  error.value = "";
  try {
    entries.value = await invoke<TsSentence[]>("load_dictionary", {
      path: dictPath.value,
    });
  } catch (e: any) {
    entries.value = [];
    error.value = e?.message ?? String(e);
  }
}
</script>

<template>
  <main class="container">
    <h1>wtool</h1>

    <form class="config" @submit.prevent="start">
      <label>
        游戏目录
        <input v-model="gameDir" />
        <button type="button" @click="pickGameDir">选择目录</button>
      </label>
      <label>
        词典文件
        <input v-model="dictPath" @input="dictManual = true" />
        <button type="button" @click="pickDict">选择文件</button>
      </label>

      <p class="hint" v-if="engineName">检测到引擎：{{ engineName }}</p>

      <div class="row">
        <button type="submit">开始翻译</button>
        <button type="button" @click="stop">停止</button>
        <button type="button" @click="load">读取词典</button>
      </div>
    </form>

    <p v-if="error" class="error">{{ error }}</p>

    <section class="inject">
      <h2>注入方式</h2>
      <p v-if="!methods.length" class="hint">
        这个游戏上还没有可用的注入方式。
      </p>
      <table v-else>
        <thead>
          <tr>
            <th>方式</th>
            <th>状态</th>
            <th>说明</th>
            <th>操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="m in methods" :key="m.id">
            <td>{{ m.name }}</td>
            <td>{{ STATE_TEXT[m.state] }}</td>
            <td class="detail">
              {{ m.detail }}
              <template v-if="m.state !== 'not_applicable'">
                <br />撤销：{{ REVERSIBLE_TEXT[m.reversible] }}
                <template v-if="m.restart_required">，装完要重启游戏</template>
              </template>
            </td>
            <td class="ops">
              <button type="button" :disabled="!canInstall(m)" @click="installMethod(m.id)">
                注入
              </button>
              <button
                type="button"
                :disabled="!canUninstall(m)"
                @click="uninstallMethod(m.id)"
              >
                取消注入
              </button>
            </td>
          </tr>
        </tbody>
      </table>
    </section>

    <pre v-if="logs.length" class="logs">{{ logs.join("\n") }}</pre>

    <p v-if="entries.length">共 {{ entries.length }} 条</p>

    <table v-if="entries.length">
      <thead>
        <tr>
          <th>#</th>
          <th>原文</th>
          <th>译文</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="(value,index) in entries">
          <td>{{ index+1 }}</td>
          <td>{{ value.source }}</td>
          <td>{{ value.target }}</td>
        </tr>
      </tbody>

    </table>

  </main>
</template>

<style scoped>
.container {
  padding-top: 4vh;
}

table {
  margin: 1em auto;
  border-collapse: collapse;
  font-size: 0.9em;
  text-align: left;
}

th,
td {
  border: 1px solid #cccccc80;
  padding: 0.3em 0.8em;
  vertical-align: top;
}

th {
  position: sticky;
  top: 0;
  background-color: inherit;
}

.source {
  white-space: pre-wrap;
}

.error {
  color: #d9534f;
}

.config label {
  display: block;
  margin: 0.4em 0;
}

.config input {
  width: min(70vw, 720px);
  margin-left: 0.5em;
}

.config label button {
  margin-left: 0.4em;
  padding: 0.4em 0.9em;
  font-size: 0.85em;
}

.inject table {
  margin: 0.5em auto;
  text-align: left;
}

.inject .detail {
  color: #888888;
  font-size: 0.85em;
}

.inject .ops {
  white-space: nowrap;
}

.inject .ops button {
  padding: 0.3em 0.8em;
  font-size: 0.85em;
}

.inject .ops button + button {
  margin-left: 0.4em;
}

.inject .ops button:disabled {
  cursor: default;
  opacity: 0.45;
  box-shadow: none;
}

.hint {
  color: #888888;
  font-size: 0.85em;
}

.logs {
  max-height: 12em;
  max-width: min(80vw, 900px);
  margin: 1em auto;
  padding: 0.6em 0.8em;
  overflow: auto;
  border: 1px solid #cccccc80;
  border-radius: 8px;
  font-size: 0.85em;
  text-align: left;
  white-space: pre-wrap;
}
</style>
<style>
:root {
  font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
  font-size: 16px;
  line-height: 24px;
  font-weight: 400;

  color: #0f0f0f;
  background-color: #f6f6f6;

  font-synthesis: none;
  text-rendering: optimizeLegibility;
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
  -webkit-text-size-adjust: 100%;
}

.container {
  margin: 0;
  padding-top: 10vh;
  display: flex;
  flex-direction: column;
  justify-content: center;
  text-align: center;
}

.logo {
  height: 6em;
  padding: 1.5em;
  will-change: filter;
  transition: 0.75s;
}

.logo.tauri:hover {
  filter: drop-shadow(0 0 2em #24c8db);
}

.row {
  display: flex;
  justify-content: center;
}

a {
  font-weight: 500;
  color: #646cff;
  text-decoration: inherit;
}

a:hover {
  color: #535bf2;
}

h1 {
  text-align: center;
}

input,
button {
  border-radius: 8px;
  border: 1px solid transparent;
  padding: 0.6em 1.2em;
  font-size: 1em;
  font-weight: 500;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  transition: border-color 0.25s;
  box-shadow: 0 2px 2px rgba(0, 0, 0, 0.2);
}

button {
  cursor: pointer;
}

button:hover {
  border-color: #396cd8;
}
button:active {
  border-color: #396cd8;
  background-color: #e8e8e8;
}

input,
button {
  outline: none;
}

#path-input {
  margin-right: 5px;
}

@media (prefers-color-scheme: dark) {
  :root {
    color: #f6f6f6;
    background-color: #2f2f2f;
  }

  a:hover {
    color: #24c8db;
  }

  input,
  button {
    color: #ffffff;
    background-color: #0f0f0f98;
  }
  button:active {
    background-color: #0f0f0f69;
  }
}

</style>