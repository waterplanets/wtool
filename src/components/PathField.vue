<script setup lang="ts">
//! 一个「带选择按钮的路径输入框」。
//!
//! 界面上路径字段到处都是，行为却只有三种（挑目录 / 挑已有文件 / 指定输出文件），
//! 所以收成一个组件：省得每个面板各写一遍 `open()` `save()` 的参数。
//!
//! 用户手工粘进来的路径可能带引号，**这里不做处理** —— 后端 `clean_path`
//! 统一收拾（`set_config` 时），前端再擦一遍反而两处逻辑。

import { computed } from "vue";
import { open, save } from "@tauri-apps/plugin-dialog";

const props = defineProps<{
  modelValue: string;
  label: string;
  placeholder?: string;
  /** `dir` 挑目录；`open` 挑已存在的文件；`save` 指定（可新建的）输出文件。 */
  mode: "dir" | "open" | "save";
  filters?: { name: string; extensions: string[] }[];
  /** `save` 模式的建议落点（一般是当前值）。 */
  defaultPath?: string;
}>();

const emit = defineEmits<{ (e: "update:modelValue", v: string): void }>();

const value = computed({
  get: () => props.modelValue,
  set: (v: string) => emit("update:modelValue", v),
});

const buttonText = computed(() =>
  props.mode === "dir" ? "选择目录" : props.mode === "save" ? "指定文件" : "选择文件",
);

async function browse() {
  const picked =
    props.mode === "dir"
      ? await open({ directory: true, multiple: false })
      : props.mode === "save"
        ? await save({
            defaultPath: props.defaultPath || undefined,
            filters: props.filters,
          })
        : await open({ multiple: false, filters: props.filters });

  if (typeof picked === "string") value.value = picked;
}
</script>

<template>
  <div class="field">
    <label>
      <span class="name">{{ label }}</span>
      <div class="line">
        <input
          v-model="value"
          type="text"
          :placeholder="placeholder"
          spellcheck="false"
        />
        <button type="button" @click="browse">{{ buttonText }}</button>
      </div>
    </label>
    <p v-if="$slots.hint" class="hint"><slot name="hint" /></p>
  </div>
</template>

<style scoped>
.field {
  margin-bottom: 12px;
}

.name {
  display: block;
  margin-bottom: 5px;
  font-size: 12.5px;
  color: var(--text-dim);
}

.line {
  display: flex;
  gap: 8px;
}

.line input {
  flex: 1;
  min-width: 0;
}

.line button {
  white-space: nowrap;
  flex: none;
}

.hint {
  margin-top: 5px;
}
</style>