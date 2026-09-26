<script setup lang="ts">
//! 采集页：攒「待翻译的原文」。两条路差得很远，所以并排放、各自说清楚。
//!
//! - 在线采集：开着游戏玩一遍，游戏侧把真跑到的文本推上来（含脚本拼出来的、
//!   `\v[n]` 展开后的真话）。**要服务**，也就只有要服务的引擎能做。
//! - 离线扫描：不开游戏，直接读游戏目录里的数据文件。谁都能做，但只能拿到
//!   静态文本，而且 RGSS3 要读归档、会慢。

import { computed } from "vue";

import * as api from "../api";
import PathField from "./PathField.vue";
import {
  config,
  engine,
  engineKnown,
  needsServer,
  running,
  scanning,
  reset,
  flushConfig,
  fail,
  say,
  stop,
} from "../store";

/** 在线采集能不能用：要服务的引擎才行。 */
const onlineOk = computed(() => needsServer.value);

async function collect() {
  reset();
  try {
    // 先把表单这份推给后端（不等防抖），否则后端可能还拿着旧配置。
    await flushConfig();
    await api.startCollect();
    running.value = true;
    say("在线采集已开始。启动游戏正常游玩，玩完点「停止」保存。");
  } catch (e) {
    fail(e);
  }
}

async function scan() {
  reset();
  scanning.value = true;
  try {
    say(await api.collectOffline(config.game_dir, config.corpus_path, config.corpus_format));
  } catch (e) {
    fail(e);
  } finally {
    scanning.value = false;
  }
}
</script>

<template>
  <div class="panel">
    <section class="card">
      <h2>语料文件</h2>

      <PathField
        v-model="config.corpus_path"
        label="写到哪个文件"
        mode="save"
        placeholder="攒到的原文写到这里"
        :defaultPath="config.corpus_path || 'corpus.tsv'"
        :filters="[
          { name: 'wtool 语料（TSV）', extensions: ['tsv', 'txt'] },
          { name: 'wtool 语料（JSON）', extensions: ['json'] },
        ]"
      />

      <div class="field">
        <label>
          <span class="name">文件格式</span>
          <select v-model="config.corpus_format">
            <option value="tsv">行式文本（TSV）—— 一行一条，能手改、能 diff</option>
            <option value="json">JSON —— 带出现次数，适合程序读</option>
          </select>
        </label>
      </div>

      <p class="hint">
        格式只在新建这个文件时起作用；文件已经存在就沿用它原来的格式，免得把攒了
        半天的东西重排一遍。想给已有语料换格式，去「词典与格式」页另存为。
      </p>
    </section>

    <section class="card">
      <div class="head">
        <h2>在线采集</h2>
        <span class="badge" :class="onlineOk ? 'ok' : 'warn'">
          {{ onlineOk ? "本引擎可用" : "本引擎不适用" }}
        </span>
      </div>

      <p class="muted">
        启动游戏正常玩一遍，剧情/菜单原文会边玩边落进语料文件 ——
        包括离线扫不到的动态文本（脚本拼的消息、变量展开后的真话）。
      </p>

      <p v-if="!onlineOk" class="hint warn-text">
        这个引擎的钩子在游戏进程里自己查表，不向 wtool 回报文本，所以没法在线采集。
        用下面的「离线扫描」收语料即可（本引擎的静态文本已经很全）。
      </p>

      <div class="actions">
        <button
          class="primary"
          :disabled="!engineKnown || !onlineOk || running"
          @click="collect"
        >
          开始在线采集
        </button>
        <button :disabled="!running" @click="stop">停止并保存</button>
      </div>
    </section>

    <section class="card">
      <h2>离线扫描</h2>
      <p class="muted">
        不开游戏，直接读游戏目录里的数据文件，把所有静态文本收进语料。目标文件
        不存在就直接生成；已存在则整体覆盖成这次扫描的结果。
      </p>
      <p v-if="engine?.known && engine.name.includes('VX')" class="hint">
        RGSS3 的文本打包在 <code>Game.rgss3a</code> 里（大包可能上 GB），
        扫描要解归档，慢是正常的。扫完一次就够了，语料可以留着反复用。
      </p>

      <div class="actions">
        <button
          class="primary"
          :disabled="!engineKnown || scanning"
          @click="scan"
        >
          {{ scanning ? "扫描中…" : "离线扫描" }}
        </button>
      </div>
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

.name {
  display: block;
  margin-bottom: 5px;
  font-size: 12.5px;
  color: var(--text-dim);
}

.muted {
  margin-bottom: 10px;
}

.warn-text {
  color: var(--warn);
  margin-bottom: 10px;
}

.actions {
  display: flex;
  gap: 10px;
  margin-top: 14px;
}
</style>