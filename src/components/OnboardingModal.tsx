import type { ReactNode } from "react";

type Props = {
  open: boolean;
  onClose: () => void;
  onOpenSources: () => void;
  onImportLocal: () => void;
};

export function OnboardingModal({ open, onClose, onOpenSources, onImportLocal }: Props) {
  if (!open) return null;
  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={(event) => {
      if (event.target === event.currentTarget) onClose();
    }}>
      <section className="config-modal onboarding-modal" role="dialog" aria-modal="true" aria-label="欢迎使用 GXPlayer">
        <div className="section-heading">
          <div>
            <p className="eyebrow">WELCOME</p>
            <h3>欢迎使用 GXPlayer</h3>
            <p>本地听歌开箱即用；整首在线播放需要你自己导入 LX 音源脚本（程序不内置可播源）。</p>
          </div>
          <button type="button" onClick={onClose} aria-label="关闭引导">×</button>
        </div>
        <ol className="onboarding-steps">
          <Step n="1" title="导入本地音乐">
            在探索页或本地曲库选择音频文件，播放全程走 Rust 内核，不经过 WebView。
          </Step>
          <Step n="2" title="（可选）导入在线音源">
            打开「音源管理」，导入社区 LX 脚本，或把 <code>.js</code> 放到 AppData 的 sources\drop-in 目录。
          </Step>
          <Step n="3" title="搜索与解析">
            搜索点歌时只解析当前这一首；失败可取消、会说明原因，不会批量请求音源。
          </Step>
          <Step n="4" title="缓存与离线">
            完整播完的在线歌会进入「离线/缓存」，下次可秒开；不会预下载。
          </Step>
        </ol>
        <div className="modal-actions">
          <button type="button" onClick={onImportLocal}>导入本地音乐</button>
          <button type="button" className="primary" onClick={onOpenSources}>去音源管理</button>
          <button type="button" onClick={onClose}>开始使用</button>
        </div>
      </section>
    </div>
  );
}

function Step({ n, title, children }: { n: string; title: string; children: ReactNode }) {
  return (
    <li>
      <span className="onboarding-n">{n}</span>
      <div>
        <strong>{title}</strong>
        <p>{children}</p>
      </div>
    </li>
  );
}
