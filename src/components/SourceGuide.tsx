/** Static adaptation + resolve-flow explainer for the sources page. */

export function SourceGuide() {
  return (
    <section className="source-guide panel-enter">
      <div className="section-heading">
        <div>
          <p className="eyebrow">HOW IT WORKS</p>
          <h3>音源适配与解析流程</h3>
          <p>GXPlayer 不内置可播源。脚本跑在独立沙箱里，只有你主动点播时才解析当前这一首。</p>
        </div>
      </div>
      <div className="source-adapt-grid">
        <article>
          <strong>已做适配</strong>
          <ul>
            <li>LX 社区脚本沙箱（crypto / HTTP / musicUrl）</li>
            <li>酷狗 / 酷我 / 网易云元数据身份映射</li>
            <li>音质偏好与逐档回退（flac24 → flac → 320k → 128k）</li>
            <li>缓存命中秒开；失败可取消与原因分类</li>
          </ul>
        </article>
        <article>
          <strong>解析流程（点歌时）</strong>
          <ol className="source-flow">
            <li>读取曲目元数据（CatalogTrack）</li>
            <li>若已有对应音质缓存 → 直接本地播</li>
            <li>否则沙箱调用 musicUrl 拿直链（仅当前曲）</li>
            <li>Rust 引擎流式播放，边播边写缓存</li>
            <li>失败则跳过 / 提示原因，不批量请求</li>
          </ol>
        </article>
      </div>
      <p className="source-guide-tip">
        也可把 <code>.js</code> 放到
        {" "}
        <code>%APPDATA%\com.gxplayer.desktop\sources\drop-in</code>
        ，启动时会自动扫描导入。
      </p>
    </section>
  );
}
