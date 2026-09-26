<script setup lang="ts">
//! 离线翻译页：语料 → 词典。
//!
//! 这一页不碰游戏，纯粹跑翻译后端，所以它跟引擎无关 —— 也是唯一能「翻一半
//! 停下来、下次接着翻」的地方（断点续传：已翻过的原文会跳过）。

import { computed } from "vue";

import * as api from "../api";
import PathField from "./PathField.vue";
import { config, running, reset, flushConfig, fail, say, stop } from "../store";

const isLlm = computed(() => config.translate_backend === "llm");

async function start() {
  reset();
  try {
    // 先把表单这份推给后端（不等防抖），否则后端可能还拿着旧配置。
    await flushConfig();
    await api.startTranslateFile();
    running.value = true;
    say("离线翻译已开始，进度见下方日志。");
  } catch (e) {
    fail(e);
  }
}
</script>

<template>
  <div class="panel">
    <section class="card">
      <h2>翻译后端</h2>

      <div class="field">
        <label>
          <span class="name">用哪个</span>
          <select v-model="config.translate_backend">
            <option value="gtx">Google gtx —— 免费、零配置</option>
            <option value="llm">LLM —— OpenAI 兼容接口（要 API Key）</option>
          </select>
        </label>
      </div>

      <p v-if="!isLlm" class="hint">
        gtx 走境外谷歌接口，国内直连可能不通。必要时设环境变量
        <code>HTTPS_PROXY</code> 再启动 wtool。
      </p>

      <template v-else>
        <div class="grid">
          <div class="field">
            <label>
              <span class="name">接口地址</span>
              <input v-model="config.llm_base_url" placeholder="https://api.deepseek.com" />
            </label>
          </div>
          <div class="field">
            <label>
              <span class="name">模型名</span>
              <input v-model="config.llm_model" placeholder="deepseek-chat" />
            </label>
          </div>
        </div>

        <div class="field">
          <label>
            <span class="name">API Key</span>
            <input v-model="config.llm_api_key" type="password" placeholder="sk-…" />
          </label>
        </div>

        <div class="field narrow">
          <label>
            <span class="name">每批条数</span>
            <input v-model.number="config.llm_batch" type="number" min="1" />
          </label>
        </div>

        <p class="hint">
          Key 存在配置文件里（<code>%APPDATA%\wtool\config.json</code>，明文，仅本机）。
          这里留空时会去读环境变量 <code>WTOOL_LLM_KEY</code> —— 不想让它落盘就用后者。
        </p>
      </template>
    </section>

    <section class="card">
      <h2>输入与输出</h2>

      <PathField
        v-model="config.corpus_path"
        label="语料文件（输入）"
        mode="open"
        placeholder="先前采集 / 扫描得到的语料"
        :filters="[{ name: '语料', extensions: ['tsv', 'json', 'txt', 'corpus'] }]"
      />

      <PathField
        v-model="config.out_path"
        label="输出词典"
        mode="save"
        placeholder="翻好的词典写到这里"
        :defaultPath="config.out_path || 'dict.tsv'"
        :filters="[
          { name: 'wtool 词典（TSV）', extensions: ['tsv', 'txt'] },
          { name: 'wtool 词典（JSON）', extensions: ['json'] },
        ]"
      />

      <div class="field">
        <label>
          <span class="name">输出格式</span>
          <select v-model="config.dict_format">
            <option value="tsv">行式文本（TSV）</option>
            <option value="json">wtool JSON</option>
            <option value="mtool">MTool 词典（扁平 JSON）</option>
          </select>
        </label>
      </div>

      <p class="hint">
        输出格式只在新建这个文件时起作用。文件已存在就沿用它的格式，
        这样每次跑都在往下接着翻，不会把之前翻的丢掉。已经存在的词典想换个格式，
        去「词典与格式」页另存为。
      </p>

      <div class="actions">
        <button class="primary" :disabled="running" @click="start">开始翻译</button>
        <button :disabled="!running" @click="stop">停止</button>
      </div>

      <p class="hint">
        可以反复跑：已翻过的原文会被跳过，越跑越全。中途点「停止」也不会丢进度，
        当前这批翻完落盘就停。
      </p>
    </section>
  </div>
</template>

<style scoped>
.panel {
  display: flex;
  flex-direction: column;
  gap: var(--gap);
}

.name {
  display: block;
  margin-bottom: 5px;
  font-size: 12.5px;
  color: var(--text-dim);
}

.grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}

.field {
  margin-bottom: 12px;
}

.narrow {
  max-width: 180px;
}

.actions {
  display: flex;
  gap: 10px;
  margin-top: 14px;
}
</style>