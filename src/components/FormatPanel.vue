<script setup lang="ts">
//! 词典与格式页：另存为 / 换格式，以及看词表。
//!
//! 「另存为」是这个页面的主角，因为有两件事只有它能做：
//!   · MTool 词典是只读契约 —— 想改它、想换格式，只能另存成 wtool 格式；
//!   · 语料换个格式不该重扫一遍游戏（RGSS3 那要读上 GB 的归档）。
//!
//! 转换只做「读进来、按目标格式写出去」，不改内容。

import { computed, ref } from "vue";

import * as api from "../api";
import PathField from "./PathField.vue";
import { config, fail, say } from "../store";

/** 语料转换的三件套。 */
const corpusSrc = ref("");
const corpusDst = ref("");
const corpusFmt = ref("json");

/** 词典转换的三件套。 */
const dictSrc = ref("");
const dictDst = ref("");
const dictFmt = ref("tsv");

/** 词表查看。 */
const viewPath = ref("");
const entries = ref<api.TsSentence[]>([]);
const viewing = ref(false);

/** 转换结果各一行提示。 */
const corpusMsg = ref("");
const dictMsg = ref("");

/** 把扩展名换成目标格式的，给「另存为」一个像样的默认落点。 */
function withExt(path: string, ext: string) {
  if (!path) return "";
  const base = path.replace(/\.[^./\\]+$/, "");
  return `${base}.${ext}`;
}

const corpusDstHint = computed(() => withExt(corpusSrc.value, corpusFmt.value));
const dictDstHint = computed(() => withExt(dictSrc.value, dictFmt.value));

async function saveCorpus() {
  corpusMsg.value = "";
  try {
    const dst = corpusDst.value || corpusDstHint.value;
    corpusMsg.value = await api.convertCorpus(corpusSrc.value, dst, corpusFmt.value);
    corpusDst.value = dst;
    say(corpusMsg.value);
  } catch (e) {
    fail(e);
  }
}

async function saveDict() {
  dictMsg.value = "";
  try {
    const dst = dictDst.value || dictDstHint.value;
    dictMsg.value = await api.convertDict(dictSrc.value, dst, dictFmt.value);
    dictDst.value = dst;
    say(dictMsg.value);
  } catch (e) {
    fail(e);
  }
}

async function readDict() {
  entries.value = [];
  viewing.value = true;
  try {
    entries.value = await api.loadDictionary(viewPath.value);
  } catch (e) {
    fail(e);
  } finally {
    viewing.value = false;
  }
}
</script>

<template>
  <div class="panel">
    <section class="card">
      <h2>语料另存为</h2>
      <p class="muted">
        把一份语料换成另一种格式。不重扫游戏，只是读进来再写出去。
      </p>

      <PathField
        v-model="corpusSrc"
        label="源文件"
        mode="open"
        placeholder="要转换的语料"
        :filters="[{ name: '语料', extensions: ['tsv', 'json', 'txt', 'corpus'] }]"
      />

      <div class="grid">
        <div class="field">
          <label>
            <span class="name">要转成</span>
            <select v-model="corpusFmt">
              <option value="tsv">行式文本（TSV）</option>
              <option value="json">JSON</option>
            </select>
          </label>
        </div>
      </div>

      <PathField
        v-model="corpusDst"
        label="另存为"
        mode="save"
        :placeholder="corpusDstHint || '目标文件'"
        :defaultPath="corpusDstHint"
        :filters="[
          { name: 'wtool 语料（TSV）', extensions: ['tsv', 'txt'] },
          { name: 'wtool 语料（JSON）', extensions: ['json'] },
        ]"
      >
        <template #hint>留空就按源文件名换扩展名，写成 {{ corpusDstHint || "…" }}。</template>
      </PathField>

      <div class="actions">
        <button class="primary" :disabled="!corpusSrc" @click="saveCorpus">转换 / 另存为</button>
      </div>
      <p v-if="corpusMsg" class="ok-text">{{ corpusMsg }}</p>
    </section>

    <section class="card">
      <h2>词典另存为</h2>
      <p class="muted">
        源文件可以是 MTool 词典 —— 那种是社区共用的只读格式，wtool 只读不写；
        想改它、想给 wtool 接着往下翻，就另存成 wtool 格式。
      </p>

      <PathField
        v-model="dictSrc"
        label="源文件"
        mode="open"
        placeholder="MTool 词典，或 wtool 词典"
        :filters="[{ name: '词典', extensions: ['json', 'tsv', 'txt'] }]"
      />

      <div class="field">
        <label>
          <span class="name">要转成</span>
          <select v-model="dictFmt">
            <option value="tsv">行式文本（TSV）—— 可排序、可 diff、能手改</option>
            <option value="json">JSON —— 带 wtool 版本标记</option>
          </select>
        </label>
      </div>

      <PathField
        v-model="dictDst"
        label="另存为"
        mode="save"
        :placeholder="dictDstHint || '目标文件'"
        :defaultPath="dictDstHint"
        :filters="[
          { name: 'wtool 词典（TSV）', extensions: ['tsv', 'txt'] },
          { name: 'wtool 词典（JSON）', extensions: ['json'] },
        ]"
      >
        <template #hint>留空就按源文件名换扩展名，写成 {{ dictDstHint || "…" }}。</template>
      </PathField>

      <div class="actions">
        <button class="primary" :disabled="!dictSrc" @click="saveDict">转换 / 另存为</button>
      </div>
      <p v-if="dictMsg" class="ok-text">{{ dictMsg }}</p>
    </section>

    <section class="card">
      <h2>查看词表</h2>
      <PathField
        v-model="viewPath"
        label="词典文件"
        mode="open"
        placeholder="当前翻译在用的词典（也可直接看界面配置的那个）"
        :filters="[{ name: '词典', extensions: ['json', 'tsv', 'txt', 'bin'] }]"
      >
        <template #hint>
          正在用的词典是 <code>{{ config.dict_path || "（还没选）" }}</code>。
        </template>
      </PathField>

      <div class="actions">
        <button class="primary" :disabled="!viewPath || viewing" @click="readDict">
          {{ viewing ? "读取中…" : "读取词表" }}
        </button>
      </div>

      <p v-if="entries.length" class="hint">共 {{ entries.length }} 条</p>
      <div v-if="entries.length" class="table-wrap">
        <table>
          <thead>
            <tr>
              <th class="num">#</th>
              <th>原文</th>
              <th>译文</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(row, i) in entries" :key="i">
              <td class="num">{{ i + 1 }}</td>
              <td class="src">{{ row.source }}</td>
              <td>{{ row.target }}</td>
            </tr>
          </tbody>
        </table>
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

.name {
  display: block;
  margin-bottom: 5px;
  font-size: 12.5px;
  color: var(--text-dim);
}

.muted {
  margin-bottom: 14px;
}

.field {
  margin-bottom: 12px;
}

.grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}

.actions {
  display: flex;
  gap: 10px;
  margin-top: 14px;
}

.ok-text {
  margin-top: 10px;
  color: var(--ok);
  font-size: 12.5px;
}

.table-wrap {
  max-height: 320px;
  overflow: auto;
  margin-top: 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
}

table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12.5px;
}

th,
td {
  padding: 6px 10px;
  text-align: left;
  vertical-align: top;
  border-bottom: 1px solid var(--border);
}

th {
  position: sticky;
  top: 0;
  background: var(--surface-2);
  color: var(--text-dim);
  font-weight: 600;
}

.num {
  width: 3.5em;
  color: var(--text-faint);
}

.src {
  white-space: pre-wrap;
  color: var(--text-dim);
}
</style>