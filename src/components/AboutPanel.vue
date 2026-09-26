<script setup lang="ts">
//! 关于页：把许可、第三方依赖、文件位置和免责声明摆在一处，方便用户查到。
//!
//! 这一页是纯静态信息，不请求后端也不改配置 —— 所以没有 `api` / `store` 依赖。
//! 版本号与 `package.json` / `Cargo.toml` 保持一致，改版本时记得三处一起改。

const VERSION = "0.1.0";
</script>

<template>
  <div class="panel">
    <section class="card">
      <div class="head">
        <span class="logo">w</span>
        <div>
          <h2>wtool</h2>
          <p class="muted">RPG Maker 嵌入式实时翻译工具 · v{{ VERSION }}</p>
        </div>
      </div>
      <p class="muted">
        把译文词典送进游戏窗口，支持 RPG Maker MZ / MV / RGSS3。不修改游戏本体，
        不提供也不分发任何游戏内容或翻译文本。
      </p>
      <p class="hint">
        以 <strong>GNU GPL v3.0 或更新（GPL-3.0-or-later）</strong> 发布，
        许可全文见项目根目录的 <code>LICENSE</code>。
      </p>
    </section>

    <section class="card">
      <h2>第三方组件</h2>
      <p class="muted">本程序在其之上构建，各自的许可与版权归其作者所有：</p>
      <ul class="deps">
        <li><strong>Tauri 2</strong> —— 桌面外壳（MIT / Apache-2.0）</li>
        <li><strong>Vue 3</strong> —— 前端界面（MIT）</li>
        <li><strong>serde / serde_json</strong> —— 数据序列化（MIT / Apache-2.0）</li>
        <li><strong>reqwest</strong> —— HTTP 客户端（MIT / Apache-2.0）</li>
        <li><strong>flate2</strong> —— zlib 解压，用于读取 RGSS3 脚本归档（MIT / Apache-2.0）</li>
        <li><strong>windows-sys</strong> —— 枚举系统字体（MIT / Apache-2.0）</li>
      </ul>
      <p class="hint">
        在线翻译调用的 Google gtx 与各 OpenAI 兼容接口属于第三方服务，按其各自条
        款使用；本程序不对这些服务的内容与可用性作任何保证。
      </p>
    </section>

    <section class="card">
      <h2>文件位置</h2>
      <ul class="deps">
        <li>配置：<code>%APPDATA%\wtool\config.json</code>（API Key 明文，仅本机）</li>
        <li>常驻主机日志：<code>%APPDATA%\wtool\host.log</code></li>
        <li>游戏侧日志：游戏目录下 <code>wtool.log</code>（取消注入时一并清理）</li>
      </ul>
    </section>

    <section class="card">
      <h2>免责声明</h2>
      <p class="muted">
        本工具仅供翻译学习与个人使用。游戏本体、角色、文本与图片的著作权归原作者
        所有，翻译词典的著作权归其译者所有。请确保你对所使用的游戏与词典拥有合法
        使用权并遵守其许可条款。使用本工具产生的任何后果由使用者自行承担。
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

.head {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
}

.logo {
  width: 40px;
  height: 40px;
  flex: none;
  display: grid;
  place-items: center;
  border-radius: 11px;
  background: var(--accent);
  color: #fff;
  font-weight: 700;
  font-size: 20px;
}

.head h2 {
  font-size: 16px;
}

.muted {
  color: var(--text-dim);
}

.deps {
  margin: 0;
  padding-left: 18px;
  font-size: 12.5px;
  color: var(--text-dim);
  line-height: 1.9;
}

.deps code,
.hint code {
  font-family: var(--font-mono);
}
</style>
