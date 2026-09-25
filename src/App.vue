<script setup lang="ts">
//! 应用外壳：左侧导航 + 右侧内容 + 底部日志。
//!
//! 五个页面的分工是照着「用户实际要干的事」分的，不是照着代码模块分的：
//!
//!   翻译      —— 主流程（选游戏 → 选词典 → 开始），一体化，不问引擎
//!   采集语料  —— 攒原文（在线采集 / 离线扫描）
//!   离线翻译  —— 语料翻成词典（唯一能断点续传的地方）
//!   词典与格式 —— 另存为 / 换格式、看词表
//!   高级      —— 注入方式手动档、启动脚本、东西都存在哪
//!
//! 日志是全局面板而不是某一页的东西：三个任务都可能往它写，用户来回切页也
//! 不该把日志丢了。

import { computed, markRaw, nextTick, onMounted, ref, watch } from "vue";

import AdvancedPanel from "./components/AdvancedPanel.vue";
import CollectPanel from "./components/CollectPanel.vue";
import FlowPanel from "./components/FlowPanel.vue";
import FormatPanel from "./components/FormatPanel.vue";
import OfflinePanel from "./components/OfflinePanel.vue";
import { init, engine, error, logs, running, stop } from "./store";

const TABS = [
  { id: "flow", label: "翻译", hint: "选游戏 → 开始", view: markRaw(FlowPanel) },
  { id: "collect", label: "采集语料", hint: "攒待翻的原文", view: markRaw(CollectPanel) },
  { id: "offline", label: "离线翻译", hint: "语料 → 词典", view: markRaw(OfflinePanel) },
  { id: "format", label: "词典与格式", hint: "另存为 / 看词表", view: markRaw(FormatPanel) },
  { id: "advanced", label: "高级", hint: "注入 / 脚本 / 路径", view: markRaw(AdvancedPanel) },
];

const active = ref("flow");
const current = computed(() => TABS.find((t) => t.id === active.value) ?? TABS[0]);

/** 日志面板收起来了没有。默认开着 —— 出问题时第一眼就该看到它。 */
const logsOpen = ref(true);
const logBox = ref<HTMLElement | null>(null);

// 新日志进来就滚到底，不然用户得自己往下拖。
watch(
  () => logs.value.length,
  async () => {
    if (!logsOpen.value) return;
    await nextTick();
    if (logBox.value) logBox.value.scrollTop = logBox.value.scrollHeight;
  },
);

/** 状态栏那句「现在在干什么」。 */
const statusText = computed(() => {
  if (running.value) return "任务运行中";
  return "空闲";
});

onMounted(init);
</script>

<template>
  <div class="app">
    <aside class="sidebar">
      <div class="brand">
        <span class="logo">w</span>
        <div>
          <div class="title">wtool</div>
          <div class="sub">游戏翻译工具</div>
        </div>
      </div>

      <nav>
        <button
          v-for="t in TABS"
          :key="t.id"
          class="nav-item"
          :class="{ active: active === t.id }"
          @click="active = t.id"
        >
          <span class="nav-label">{{ t.label }}</span>
          <span class="nav-hint">{{ t.hint }}</span>
        </button>
      </nav>

      <div class="engine-chip" :class="{ known: engine?.known }">
        <template v-if="engine?.known">
          <div class="en-name">{{ engine.name }}</div>
          <div class="en-rt">{{ engine.runtime }}</div>
        </template>
        <template v-else>
          <div class="en-name">未识别引擎</div>
          <div class="en-rt">去「翻译」页填游戏目录</div>
        </template>
      </div>
    </aside>

    <main>
      <header class="topbar">
        <div>
          <h1>{{ current.label }}</h1>
          <p class="hint">{{ current.hint }}</p>
        </div>
        <div class="status">
          <span class="badge" :class="running ? 'accent' : ''">{{ statusText }}</span>
          <button v-if="running" class="danger" @click="stop">停止</button>
        </div>
      </header>

      <p v-if="error" class="error-bar">{{ error }}</p>

      <div class="content">
        <component :is="current.view" />
      </div>

      <section class="logs" :class="{ closed: !logsOpen }">
        <button class="logs-head" @click="logsOpen = !logsOpen">
          <span>日志</span>
          <span class="hint">{{ logs.length }} 行 · 点击{{ logsOpen ? "收起" : "展开" }}</span>
        </button>
        <div v-show="logsOpen" ref="logBox" class="logs-body">
          <p v-if="!logs.length" class="hint empty">还没有日志。开始一个任务后，进度会显示在这里。</p>
          <pre v-else>{{ logs.join("\n") }}</pre>
        </div>
      </section>
    </main>
  </div>
</template>

<style scoped>
.app {
  display: grid;
  grid-template-columns: 220px 1fr;
  height: 100vh;
  overflow: hidden;
}

/*---------------------------------------------------------------------------
  左侧导航
---------------------------------------------------------------------------*/

.sidebar {
  display: flex;
  flex-direction: column;
  gap: 18px;
  padding: 16px 12px;
  background: var(--surface);
  border-right: 1px solid var(--border);
  overflow-y: auto;
}

.brand {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 0 6px;
}

.logo {
  width: 32px;
  height: 32px;
  flex: none;
  display: grid;
  place-items: center;
  border-radius: 9px;
  background: var(--accent);
  color: #fff;
  font-weight: 700;
  font-size: 17px;
}

.title {
  font-weight: 700;
  letter-spacing: 0.3px;
}

.sub,
.en-rt {
  font-size: 11.5px;
  color: var(--text-faint);
}

nav {
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.nav-item {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 1px;
  padding: 8px 10px;
  background: transparent;
  border: 1px solid transparent;
  border-radius: var(--radius-sm);
  text-align: left;
}

.nav-item:hover:not(.active) {
  background: var(--surface-2);
  border-color: transparent;
}

.nav-item.active {
  background: var(--accent-soft);
  border-color: color-mix(in srgb, var(--accent) 40%, transparent);
}

.nav-label {
  font-weight: 600;
}

.nav-item.active .nav-label {
  color: var(--accent);
}

.nav-hint {
  font-size: 11.5px;
  color: var(--text-faint);
}

.engine-chip {
  margin-top: auto;
  padding: 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--surface-2);
}

.engine-chip.known {
  border-color: color-mix(in srgb, var(--ok) 40%, transparent);
}

.en-name {
  font-size: 12.5px;
  font-weight: 600;
  margin-bottom: 2px;
}

/*---------------------------------------------------------------------------
  右侧主区
---------------------------------------------------------------------------*/

main {
  display: flex;
  flex-direction: column;
  min-width: 0;
  padding: 16px 20px 0;
  overflow: hidden;
}

.topbar {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 14px;
}

h1 {
  font-size: 19px;
}

.status {
  display: flex;
  align-items: center;
  gap: 8px;
}

.error-bar {
  margin-bottom: 14px;
  padding: 9px 12px;
  border-radius: var(--radius-sm);
  border: 1px solid color-mix(in srgb, var(--danger) 45%, transparent);
  background: color-mix(in srgb, var(--danger) 12%, transparent);
  color: var(--danger);
  font-size: 12.5px;
}

.content {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding-bottom: 16px;
}

/*---------------------------------------------------------------------------
  底部日志
---------------------------------------------------------------------------*/

.logs {
  flex: none;
  border-top: 1px solid var(--border);
  background: var(--surface);
  border-radius: var(--radius) var(--radius) 0 0;
}

.logs-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  width: 100%;
  padding: 8px 12px;
  background: transparent;
  border: none;
  border-radius: 0;
  font-weight: 600;
  font-size: 12.5px;
}

.logs-head:hover {
  background: var(--surface-2);
}

.logs-body {
  max-height: 190px;
  overflow: auto;
  padding: 0 12px 10px;
}

.logs-body pre {
  margin: 0;
  font-family: var(--font-mono);
  font-size: 11.5px;
  line-height: 1.6;
  white-space: pre-wrap;
  color: var(--text-dim);
}

.logs-body .empty {
  padding: 4px 0 10px;
}
</style>