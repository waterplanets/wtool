<script setup lang="ts">
//! 高级页：平常不用进来，出问题时才是排查的地方。
//!
//! 放三样东西：注入方式表（手动装/撤，看清每种方式到底往游戏目录写了什么）、
//! 启动脚本生成、以及「东西都存在哪」这张说明卡 —— 用户问「配置文件放哪、
//! key 存哪」时，答案就在最后那张卡里。

import { computed, ref } from "vue";

import * as api from "../api";
import { config, engine, methods, refreshEngine, fail, say } from "../store";

/** 手动操作的结果提示，按方式 id 分开记。 */
const msgs = ref<Record<string, string>>({});

/** 生成启动脚本的结果。 */
const launcherMsg = ref("");

const STATE_TEXT: Record<api.MethodStatus["state"], string> = {
  not_applicable: "不适用",
  ready: "未安装",
  installed: "已安装",
  stale: "需重装",
};

const MODE_TEXT: Record<api.MethodStatus["mode"], string> = {
  live: "实时连接",
  onetime: "一次性注入",
};

/** 靠什么维持通道 —— 也就是「装完之后用户还得干什么」。 */
const SERVICE_TEXT: Record<api.MethodStatus["service"], string> = {
  none: "装完即终态，不用开 wtool",
  tcp: "翻译时保持 wtool 打开（游戏连上来）",
  file_sync: "翻译时保持 wtool 打开（改字体即时生效）",
};

const REVERSE_TEXT: Record<api.MethodStatus["reversible"], string> = {
  runtime_full: "断开即恢复",
  runtime_and_offline: "断开并还原文件",
  offline_only: "只能还原文件",
  none: "只能重启游戏",
};

/** 装完要不要重启游戏。 */
const restartHint = computed(() =>
  methods.value.some((m) => m.state !== "not_applicable" && m.restart_required)
    ? "这个方法装完要重启游戏才生效。"
    : "装完启动游戏即可，不用重启系统。",
);

function canInstall(m: api.MethodStatus) {
  return m.state === "ready" || m.state === "stale";
}

function canUninstall(m: api.MethodStatus) {
  return m.state === "installed" || m.state === "stale";
}

async function install(m: api.MethodStatus) {
  msgs.value[m.id] = "";
  try {
    msgs.value[m.id] = (await api.installInjection(
      config.game_dir,
      m.id,
      config.dict_path,
      config.font,
    )).join("\n");
    say(...msgs.value[m.id].split("\n"));
  } catch (e) {
    fail(e);
  }
  await refreshEngine();
}

async function uninstall(m: api.MethodStatus) {
  msgs.value[m.id] = "";
  try {
    msgs.value[m.id] = (await api.uninstallInjection(config.game_dir, m.id)).join("\n");
    say(...msgs.value[m.id].split("\n"));
  } catch (e) {
    fail(e);
  }
  await refreshEngine();
}

async function makeLauncher() {
  launcherMsg.value = "";
  try {
    launcherMsg.value = await api.generateLauncher(config.game_dir);
    say(launcherMsg.value);
  } catch (e) {
    fail(e);
  }
}

async function removeLauncher() {
  launcherMsg.value = "";
  try {
    launcherMsg.value = await api.removeLauncher(config.game_dir);
    say(launcherMsg.value);
  } catch (e) {
    fail(e);
  }
}
</script>

<template>
  <div class="panel">
    <section class="card">
      <div class="head">
        <h2>注入方式</h2>
        <span v-if="engine" class="badge">{{ engine.name }} · {{ engine.runtime }}</span>
      </div>
      <p class="muted">
        平常走「翻译」页就够，这里是手动档：单独装、单独撤、看清每种方式往游戏目录
        写了什么。每个引擎有两条路 —— <b>实时连接</b>（调字体用）和
        <b>一次性注入</b>（正式玩用，装完即终态）。装一条会自动把另一条撤掉，
        免得在游戏里重叠挂钩。{{ restartHint }}
      </p>

      <p v-if="!methods.length" class="hint">
        这个游戏上还没有可用的注入方式 —— 多半是游戏目录填错了。
      </p>

      <table v-else>
        <thead>
          <tr>
            <th>方式</th>
            <th>状态</th>
            <th>说明</th>
            <th class="ops-col">操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="m in methods" :key="m.id">
            <td>
              <div class="mname">{{ m.name }}</div>
              <span class="badge">{{ MODE_TEXT[m.mode] }}</span>
              <span v-if="m.persistent" class="badge ok">持久</span>
              <span v-else class="badge">退出时还原</span>
            </td>
            <td>{{ STATE_TEXT[m.state] }}</td>
            <td class="detail">
              {{ m.detail }}
              <template v-if="m.state !== 'not_applicable'">
                <br />撤销：{{ REVERSE_TEXT[m.reversible] }}
                <br />服务：{{ SERVICE_TEXT[m.service] }}
              </template>
            </td>
            <td class="ops">
              <button :disabled="!canInstall(m)" @click="install(m)">注入</button>
              <button :disabled="!canUninstall(m)" @click="uninstall(m)">取消注入</button>
              <p v-if="msgs[m.id]" class="hint out">{{ msgs[m.id] }}</p>
            </td>
          </tr>
        </tbody>
      </table>
    </section>

    <section class="card">
      <h2>常驻主机启动脚本</h2>
      <p class="muted">
        给「翻译时必须开着 wtool」的引擎用：在游戏目录里写一个
        <code>wtool-host.cmd</code>，双击就能把翻译主机跑起来（无窗口），
        不用一直开着这个界面。不需要了就从这里删掉，别的一概不碰。
      </p>
      <div class="actions">
        <button class="primary" :disabled="!config.game_dir" @click="makeLauncher">
          生成到游戏目录
        </button>
        <button :disabled="!config.game_dir" @click="removeLauncher">删除</button>
      </div>
      <p v-if="launcherMsg" class="hint out">{{ launcherMsg }}</p>
    </section>

    <section class="card">
      <h2>东西都存在哪</h2>
      <dl class="paths">
        <dt>配置 + API Key</dt>
        <dd>
          <code>%APPDATA%\wtool\config.json</code>
          <span class="hint">
            —— 界面里改的每一样都存这儿，机器级、不跟着游戏走。Key 是明文的，
            不想落盘就留空、改用环境变量 <code>WTOOL_LLM_KEY</code>。
          </span>
        </dd>

        <dt>主机日志</dt>
        <dd>
          <code>%APPDATA%\wtool\host.log</code>
          <span class="hint">—— 启动脚本跑起来后，翻译过程都写在这儿。</span>
        </dd>

        <dt>注入到游戏里的东西</dt>
        <dd>
          <span class="hint">
            MV/MZ：游戏目录的 <code>js/plugins/wtool_bridge.js</code>（wtool 退出时还原），
            运行日志 <code>wtool.log</code> 与轮转的 <code>wtool.1-4.log</code> 也在游戏目录，
            点「取消注入」会连同插件一起清掉。<br />
            RGSS3：<code>Patch/</code> 下的钩子、词典与日志
            （<code>wtool_hook.log</code> / <code>wtool_live.log</code> 等）；
            点「取消注入」会连同这些日志一起清掉。
          </span>
        </dd>
      </dl>
    </section>
  </div>
</template>

<style scoped>
.panel {
  display: flex;
  flex-direction: column;
  gap: var(--gap);
}

.head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-bottom: 12px;
}

.muted {
  margin-bottom: 14px;
}

table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12.5px;
}

th,
td {
  padding: 8px 10px;
  text-align: left;
  vertical-align: top;
  border-bottom: 1px solid var(--border);
}

th {
  color: var(--text-dim);
  font-weight: 600;
}

.mname {
  font-weight: 600;
  margin-bottom: 4px;
}

.detail {
  color: var(--text-faint);
}

.ops-col {
  width: 8em;
}

.ops {
  white-space: nowrap;
}

.ops button {
  display: block;
  width: 100%;
  padding: 5px 8px;
  font-size: 12px;
}

.ops button + button {
  margin-top: 6px;
}

.out {
  margin-top: 6px;
  white-space: pre-wrap;
}

.actions {
  display: flex;
  gap: 10px;
  margin-top: 12px;
}

.paths {
  margin: 0;
}

.paths dt {
  font-weight: 600;
  margin-top: 10px;
}

.paths dt:first-child {
  margin-top: 0;
}

.paths dd {
  margin: 2px 0 0;
}

.paths .hint {
  display: inline;
}
</style>