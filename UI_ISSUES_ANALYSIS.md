# GXPlayer UI 问题分析和建议

## 问题清单

### 1. Sidebar tooltip 被遮盖
**现象**: 折叠侧边栏时鼠标悬停显示的功能提示会被其他元素遮盖

**原因**: 
- tooltip z-index: 120，可能不够高
- 其他面板（如队列面板、歌词面板）可能有更高的 z-index

**建议方案**:
```css
/* 提高 tooltip z-index 到 9999 */
.sidebar-collapsed .sidebar nav button[data-tooltip]:hover::after,
.sidebar-collapsed .sidebar nav button[data-tooltip]:focus-visible::after {
  z-index: 9999; /* 从 120 改为 9999 */
}
```

---

### 2. 侧边栏按钮不够居中
**现象**: 左侧导航按钮感觉偏右，在侧边栏里不够居中

**原因**:
- 按钮 grid 布局：`grid-template-columns: 32px 1fr`
- 图标列固定 32px，但图标本身 18px，左右各有 7px 空白
- 整体 padding: 0 10px

**建议方案 A (微调图标居中)**:
```css
.sidebar nav button,
.sidebar-playlists button {
  padding: 0 8px; /* 从 10px 改为 8px */
}
```

**建议方案 B (完全居中图标，折叠时更明显)**:
```css
.sidebar nav button span {
  font-size: 18px;
  text-align: center;
  margin-left: -2px; /* 视觉补偿 */
}
```

---

### 3. 沉浸模式音效介绍文字溢出
**现象**: 播放页沉浸模式下，音效模式说明文字跑出屏幕外

**原因**:
- `stage-panel` 在小屏下变单列布局
- DspPresetControls 内的 small 文字可能换行不当或容器未限制宽度

**建议方案**:
```css
/* 在 stage-panel 内强制限制宽度 */
.stage-panel .dsp-preset-grid button {
  min-width: 0; /* 已有 */
  max-width: 100%; /* 新增 */
}

.stage-panel .dsp-preset-grid button small {
  overflow-wrap: break-word;
  word-break: break-word;
  hyphens: auto;
}

/* 或者在小屏时调整网格 */
@media (max-width: 720px) {
  .stage-panel .dsp-preset-grid {
    grid-template-columns: repeat(auto-fill, minmax(100px, 1fr));
  }
}
```

---

### 4. 整体布局优化建议

**当前观察**:
- App.css 5419 行，包含三套完整主题定义
- 设计 token 系统完善，但所有主题内联在一个文件
- 虚拟滚动已优化至 80 阈值
- 组件提取进行中

**建议优化方向**:
1. **CSS 拆分** (优先级：中)
   - 拆出 `themes/dark.css`, `themes/light.css`, `themes/warm.css`
   - 保留 base tokens 在主文件
   - 运行时按需加载主题

2. **响应式断点统一** (优先级：高)
   - 当前有 840px, 900.98px, 1140.98px 等多个断点
   - 建议统一为 640px / 768px / 1024px / 1280px (Tailwind 标准)

3. **沉浸模式优化** (优先级：高)
   - stage-panel 在窄屏下体验不佳
   - 建议 720px 以下隐藏音效网格，改为 select 下拉

4. **减少视觉权重层级** (优先级：低)
   - 当前有 7 层 surface (`--base` 到 `--panel-strong`)
   - 考虑简化到 4-5 层以提高可辨识度

---

### 5. 窗口控制按钮位置
**现象**: 右上角三个窗口按钮被 Gemini 挪到略微偏左位置（margin-left: 6px），感觉有些奇怪

**原因**:
- 在 commit 2148ed7 (feat(ui): svg icon set, player visual refresh) 中引入
- 可能是为了视觉平衡或与其他元素对齐
- 标准 Windows 应用窗口控制按钮通常紧贴右上角

**建议**:
```css
/* 移除左边距，恢复传统位置 */
.window-controls {
  margin-left: 0; /* 从 6px 改为 0 */
}
```

**权衡**:
- ✅ 符合 Windows 应用规范
- ✅ 用户习惯（右上角最边缘）
- ⚠️ 如果之前调整是为了避免其他元素冲突，需要验证

---

## 修复优先级

1. **高优先级 (立即修复)**
   - 问题 3: 音效文字溢出（影响功能可用性）
   - 问题 5: 窗口按钮位置（用户反馈明确）

2. **中优先级 (本周内)**
   - 问题 1: tooltip 遮盖（影响可用性，但不常遇到）
   - 问题 2: 按钮居中（视觉抛光）

3. **低优先级 (后续迭代)**
   - 整体布局优化（架构改进）
