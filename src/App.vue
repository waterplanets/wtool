<script setup lang="ts">
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

/** 与 Rust 侧 model::TsSentence 一一对应 */
interface TsSentence {
  source: string;
  target: string;
}

const path = ref(
  "D:\\myProgram\\game2\\lilisproject\\SUCCUBUS DESIRE ~リリスプロジェクト~\\TrsData.bin",
);
const entries = ref<TsSentence[]>([]);
const error = ref("");

async function load() {
  error.value = "";
  try {
    entries.value = await invoke<TsSentence[]>("load_dictionary", {
      path: path.value,
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

    <form @submit.prevent="load">
      <input v-model="path" />
      <button type="submit">查询</button>
    </form>

    <p v-if="error"> {{ error }} </p>
    <p v-else> 共 {{ entries.length }} 条 </p>

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