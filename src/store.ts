//! 全局状态：几个面板（翻译/采集/离线翻译/工具）共享的那一份。
//!
//! 就一个 `reactive` 配置 + 几个 `ref`，不引状态库 —— 组件之间要共享的东西
//! 本来就少，一句 `import { config } from "../store"` 就够，比装个 store 轻。

import { computed, reactive, ref, watch } from "vue";
import { listen } from "@tauri-apps/api/event";

import * as api from "./api";

/** 当前这份配置。界面上所有输入框都绑到它的字段上，改完统一 `save()`。 */
export const config = reactive<api.AppConfig>(api.emptyConfig());

/** 日志。Rust 侧 `app.emit("trans-status", msg)`，这里攒着显示。 */
export const logs = ref<string[]>([]);

/** 最后一条错误。一次只显示一条，新的顶掉旧的。 */
export const error = ref("");

/** 有后台任务在跑。这时禁用所有「开始」按钮，免得连点起好几个。 */
export const running = ref(false);

/** 离线扫描（同步命令）在跑。和 `running` 分开：它不是「任务」，没有停止按钮。 */
export const scanning = ref(false);

/** 刚算出来的「开始」结果，决定翻译页接下来怎么提示用户。 */
export const outcome = ref<api.StartOutcome | null>(null);

/** 引擎探测结果。 */
export const engine = ref<api.EngineInfo | null>(null);

/** 这个游戏上的注入方式与状态。 */
export const methods = ref<api.MethodStatus[]>([]);

/** 本机字体清单。 */
export const systemFonts = ref<string[]>([]);

/** 引擎认出来了没有。认不出就不让开始，先让用户去改目录。 */
export const engineKnown = computed(() => engine.value?.known === true);

/**
 * 这次会真正用上的注入方式（第一条非「不适用」的），也是
 * 「开始翻译」会选的那条 —— 后端 `inject::pick` 走的是同一个优先级。
 */
export const activeMethod = computed(
  () => methods.value.find((m) => m.state !== "not_applicable") ?? null,
);

/** 翻译要不要 wtool 常驻服务。没有可用注入方式时按 true（保守：先别关）。 */
export const needsServer = computed(() => activeMethod.value?.needs_server ?? true);

/** 字体菜单的候选：系统字体 + 当前这个（换机器后原字体可能不在列表里）。 */
export const fontOptions = computed(() => {
  const list = systemFonts.value;
  const cur = config.font.trim();
  return cur && !list.includes(cur) ? [cur, ...list] : list;
});

/** 往日志里追加几行，超出上限就从头上丢。 */
export function say(...lines: string[]) {
  logs.value.push(...lines);
  if (logs.value.length > 400) {
    logs.value.splice(0, logs.value.length - 400);
  }
}

/** 记下一条错误，同时写进日志 —— 用户回头翻日志能看全。 */
export function fail(e: unknown) {
  const msg = api.messageOf(e);
  error.value = msg;
  say(`错误：${msg}`);
}

/** 清空错误与日志，开始一件新活儿之前调。 */
export function reset() {
  error.value = "";
  logs.value = [];
}

/** 读配置填进表单。开屏调一次。 */
async function loadConfig() {
  try {
    Object.assign(config, await api.loadConfig());
  } catch (e) {
    fail(e);
  }
}

/** 存配置：落盘 + 推给后端，并用后端收拾过的那份刷新表单。 */
export async function saveConfig() {
  Object.assign(config, await api.setConfig({ ...config }));
}

/**
 * 立刻把配置推给后端，不等那 500ms 防抖。
 *
 * 点「开始」之前必须调一次。表单和后端任务层各存着一份配置，后端那份只靠防抖
 * 同步，于是有两个窗口会出事：刚开界面时配置是刚读回来的、还没推；或者用户填完
 * 目录立刻就点。两种情况后端手上都是旧的那份（`game_dir` 还是空的），会报
 * 「还没填游戏目录」——框里明明有，用户只能改一个字再改回来才通。
 */
export async function flushConfig() {
  clearTimeout(saveTimer);
  saveTimer = undefined;
  await saveConfig();
}

/** 重探引擎与注入方式。游戏目录一变就调，免得对着 A 游戏的状态操作 B 游戏。 */
export async function refreshEngine() {
  try {
    engine.value = await api.detectEngine(config.game_dir);
  } catch (e) {
    engine.value = null;
    fail(e);
  }
  try {
    methods.value = await api.listInjections(config.game_dir);
  } catch (e) {
    methods.value = [];
    fail(e);
  }
}

//-------------------------------------------------------------------------
// 自动存取
//-------------------------------------------------------------------------
//
// 「配置」这件事用户不该操心：填完目录、选完词典，它就该记住。所以这里没有
// 「保存」按钮 —— 改了自动落盘（防抖 500ms），开屏自动读回来。
//
// 两处防抖/防抖外的守卫：
//   · `ready`：开屏读配置之前不存，否则会把还没填的表单覆盖掉磁盘上的值。
//   · 值没变时 Vue 不触发 watch，所以「存完回填表单」不会来回抖成死循环。

let ready = false;
let saveTimer: number | undefined;

/** 开屏初始化：读配置 + 拉字体。之后「改了自动存」才生效。 */
export async function init() {
  await loadConfig();
  void loadFonts();
  ready = true;
}

// 配置一改就落盘。`deep` 是必须的：字段都在这一层对象里。
watch(
  config,
  () => {
    if (!ready) return;
    clearTimeout(saveTimer);
    saveTimer = window.setTimeout(() => void saveConfig().catch(fail), 500);
  },
  { deep: true },
);

// 目录一变就重探引擎，开屏也探一次（immediate）——用户不用先点一下。
watch(() => config.game_dir, () => void refreshEngine(), { immediate: true });

// Rust 那边推上来的状态消息，全局只订一次。
listen<string>("trans-status", (e) => say(e.payload));

/** 拉一次系统字体清单。拉不到就空表，字体菜单退化成「不指定」。 */
export async function loadFonts() {
  try {
    systemFonts.value = await api.listSystemFonts();
  } catch {
    systemFonts.value = [];
  }
}

/** 停任务。三个任务一个意思。 */
export async function stop() {
  try {
    await api.stopTrans();
  } catch (e) {
    fail(e);
  } finally {
    running.value = false;
    outcome.value = null;
  }
}