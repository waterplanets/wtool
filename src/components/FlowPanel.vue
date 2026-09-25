<script setup lang="ts">
//! 翻译页：一体化流程。
//!
//! 这是用户抱怨的那个「大翻译按钮只有部分注入方式才有用」的解药：这一页不问
//! 引擎、不问注入方式，只走「选游戏 → 选词典 → 开始」。但引擎的差别**必须说
//! 清楚**（用户明确要求），所以在开始按钮旁边按引擎能力写出两种截然不同的
//! 后续操作 —— MV/MZ 得保持 wtool 开着，RGSS3 装完就能关。

import { computed, ref } from "vue";

import * as api from "../api";
import PathField from "./PathField.vue";
import {
  config,
  engine,
  engineKnown,
  activeMethod,
  needsServer,
  fontOptions,
  outcome,
  running,
  stop,
  reset,
  flushConfig,
  fail,
  say,
} from "../store";

/** 生启动脚本的结果（一行提示），贴在按钮下面。 */
const launcherMsg = ref("");

/**
 * 说白了就是「这个引擎点完开始之后，用户该干什么」。两种情况差得远，
 * 与其让用户去猜，不如直接写出来。
 */
const flow = computed(() => {
  if (!engineKnown.value) {
    return {
      title: "先填一个游戏目录",
      steps: [] as string[],
      note: "",
    };
  }
  if (needsServer.value) {
    return {
      title: "点开始后：wtool 要一直开着",
      steps: [
        "点「开始翻译」——wtool 会装上注入并开始监听。",
        "启动游戏，正常游玩：剧情、菜单、物品说明都会被翻成中文。",
        "翻译期间**别关 wtool**（关了桥就断了）。玩完点「停止」。",
      ],
      note: "不想一直开着这个界面，可以用下面的「生成启动脚本」：双击它就能在后台把主机跑起来，日志落在配置目录。",
    };
  }
  return {
    title: "点开始后：装完就能关掉 wtool",
    steps: [
      "点「安装并完成」——wtool 会把钩子和词典写进游戏目录。",
      "**这一步做完就可以关掉 wtool 了**，翻译在游戏进程里自己跑，不需要 wtool 陪跑。",
      "启动游戏即可看到中文。想撤掉翻译时，回到这一页点「取消注入」。",
    ],
    note: "这个引擎不需要常驻服务，所以也没有启动脚本 —— 装一次就长期有效。",
  };
});

/** 剪贴板里粘进来可能带引号，先给用户看一眼收拾后的样子。 */
const cleanHint = computed(() => {
  const raw = config.game_dir.trim();
  if (!raw) return "";
  const cleaned = raw.replace(/["“”「」]/g, "").trim();
  return cleaned === raw ? "" : `路径里的引号会被自动去掉，实际用的是：${cleaned}`;
});

async function start() {
  reset();
  launcherMsg.value = "";
  try {
    // 先把表单这份推给后端（不等防抖），否则后端可能还拿着旧配置。
    await flushConfig();
    const result = await api.startTranslation();
    outcome.value = result;
    // 不需要服务的机制（RGSS3）装完就完了，别把界面卡在「正在运行」。
    running.value = result.server_running;
    say(
      result.server_running
        ? `已开始（${result.method}）。保持 wtool 打开，去启动游戏吧。`
        : `已安装（${result.method}）。现在可以关掉 wtool，直接启动游戏。`,
    );
  } catch (e) {
    fail(e);
  }
}

/** 生成常驻主机启动脚本。只有要服务的引擎才用得上。 */
async function makeLauncher() {
  reset();
  try {
    launcherMsg.value = await api.generateLauncher(config.game_dir);
    say(launcherMsg.value);
  } catch (e) {
    fail(e);
  }
}

/** 删掉游戏目录里的启动脚本（只删这一个文件）。 */
async function removeLauncher() {
  reset();
  try {
    launcherMsg.value = await api.removeLauncher(config.game_dir);
    say(launcherMsg.value);
  } catch (e) {
    fail(e);
  }
}
</script>

<template>
  <div class="flow">
    <!-- 第一步：游戏目录 + 引擎识别 -->
    <section class="card">
      <div class="head">
        <h2>1. 选择游戏</h2>
        <span v-if="engine" class="badge" :class="engineKnown ? 'ok' : 'warn'">
          {{ engineKnown ? engine.name : "认不出引擎" }}
        </span>
      </div>

      <PathField
        v-model="config.game_dir"
        label="游戏目录"
        mode="dir"
        placeholder="Game.exe 所在的目录"
      >
        <template #hint>
          <template v-if="cleanHint">{{ cleanHint }}<br /></template>
          <template v-if="engine">
            {{ engine.name }}（{{ engine.runtime }}）
            <template v-if="!engineKnown">
              —— 这个目录里没找到任何引擎的特征文件，检查一下是不是填错层了。
            </template>
          </template>
          <template v-else>填好目录后会自动识别引擎。</template>
        </template>
      </PathField>
    </section>

    <!-- 第二步：词典 + 字体 -->
    <section class="card">
      <h2>2. 选择词典与字体</h2>

      <PathField
        v-model="config.dict_path"
        label="词典文件"
        mode="open"
        placeholder="MTool 的 TrsData.json，或 wtool 生成的词典"
        :filters="[{ name: '词典', extensions: ['json', 'tsv', 'bin', 'txt'] }]"
      >
        <template #hint>
          MTool 词典只读：能拿来翻译，但 wtool 不会往里写。想改动或换个格式，
          去「词典与格式」页另存为 wtool 格式。
        </template>
      </PathField>

      <div class="field">
        <label>
          <span class="name">目标字体</span>
          <select v-model="config.font">
            <option value="">不指定（保持游戏原字体）</option>
            <option v-for="f in fontOptions" :key="f" :value="f">{{ f }}</option>
          </select>
        </label>
      </div>

      <p class="hint">
        字体改动不会立刻生效：需要重新点一次「开始」并重启游戏。不指定时，RGSS3
        用内置黑体保证中文能画出来，MV/MZ 保持游戏原字体。
      </p>
    </section>

    <!-- 第三步：开始 -->
    <section class="card start-card">
      <div class="head">
        <h2>3. 开始</h2>
        <span v-if="activeMethod" class="badge accent">
          {{ activeMethod.name }}
          <template v-if="activeMethod.persistent">（持久）</template>
        </span>
      </div>

      <p class="lead">{{ flow.title }}</p>
      <ol class="steps">
        <li v-for="(s, i) in flow.steps" :key="i">{{ s }}</li>
      </ol>
      <p v-if="flow.note" class="hint">{{ flow.note }}</p>

      <div class="actions">
        <button
          class="primary wide"
          :disabled="running || !engineKnown"
          @click="start"
        >
          {{ needsServer ? "开始翻译" : "安装并完成" }}
        </button>
        <button v-if="running" class="wide" @click="stop">停止</button>
      </div>

      <div v-if="needsServer && engineKnown" class="sub">
        <button @click="makeLauncher">生成启动脚本（不想一直开着界面时）</button>
        <button @click="removeLauncher">删除启动脚本</button>
        <p v-if="launcherMsg" class="hint">{{ launcherMsg }}</p>
      </div>

      <p v-if="!engineKnown" class="hint">
        引擎还没认出来，先把游戏目录填对。
      </p>

      <!-- 开始之后：把「现在该干什么」顶到最显眼的地方 -->
      <div v-if="outcome" class="after" :class="{ live: outcome.server_running }">
        <strong v-if="outcome.server_running">翻译进行中</strong>
        <strong v-else>已完成</strong>
        <span v-if="outcome.server_running">
          —— 去启动游戏，并保持 wtool 打开。玩完回到这里点「停止」。
        </span>
        <span v-else>—— 现在就能关掉 wtool，启动游戏即可看到中文。</span>
      </div>
    </section>
  </div>
</template>

<style scoped>
.flow {
  display: flex;
  flex-direction: column;
  gap: var(--gap);
}

.head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-bottom: 14px;
}

.name {
  display: block;
  margin-bottom: 5px;
  font-size: 12.5px;
  color: var(--text-dim);
}

.start-card .lead {
  font-weight: 600;
  margin-bottom: 8px;
}

.steps {
  margin: 0 0 10px;
  padding-left: 20px;
  color: var(--text-dim);
}

.steps li {
  margin-bottom: 4px;
}

.actions {
  display: flex;
  gap: 10px;
  margin-top: 16px;
}

.actions .wide {
  flex: 1;
}

.sub {
  margin-top: 12px;
  padding-top: 12px;
  border-top: 1px dashed var(--border);
}

.actions + .hint,
.sub p.hint {
  margin-top: 8px;
}

.after {
  margin-top: 14px;
  padding: 10px 12px;
  border-radius: var(--radius-sm);
  background: var(--surface-2);
  border: 1px solid var(--border);
  font-size: 13px;
}

.after.live {
  border-color: color-mix(in srgb, var(--accent) 50%, transparent);
  background: var(--accent-soft);
}

.after strong {
  margin-right: 4px;
}
</style>