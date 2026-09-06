import type { ComponentType, SVGProps } from "react";
import { Compass, Library, Clock3, Heart, Radio, Settings2, Play, Pause, SkipBack, SkipForward, ListMusic, ArrowLeft, Search, Menu, Palette, Minus, Square, Copy, X, List, Repeat, Repeat1, Shuffle, Volume1, Volume2, VolumeX } from "lucide-react";

export type IconProps = SVGProps<SVGSVGElement> & { size?: number };
function wrap(Component: ComponentType<IconProps>, defaultSize: number) {
  return function Wrapped({ size = defaultSize, ...props }: IconProps) { return <Component size={size} {...props} />; };
}

export const IconDiscovery = wrap(Compass, 18);
export const IconLibrary = wrap(Library, 18);
export const IconHistory = wrap(Clock3, 18);
export function IconFavorites({ size = 18, filled = false, ...props }: IconProps & { filled?: boolean }) { return <Heart size={size} fill={filled ? "currentColor" : "none"} {...props} />; }
export const IconSources = wrap(Radio, 18);
export const IconSettings = wrap(Settings2, 18);
export const IconPlay = wrap(Play, 16);
export const IconPause = wrap(Pause, 16);
export const IconPrev = wrap(SkipBack, 16);
export const IconNext = wrap(SkipForward, 16);
export const IconModeSeq = wrap(List, 16);
export const IconModeAll = wrap(Repeat, 16);
export const IconModeOne = wrap(Repeat1, 16);
export const IconModeShuf = wrap(Shuffle, 16);
export function IconVolume({ size = 16, volume = 1, ...props }: IconProps & { volume?: number }) { const Volume = volume < 0.001 ? VolumeX : volume < 0.5 ? Volume1 : Volume2; return <Volume size={size} {...props} />; }
export const IconQueue = wrap(ListMusic, 16);
export const IconBack = wrap(ArrowLeft, 18);
export const IconSearch = wrap(Search, 16);
export const IconMenu = wrap(Menu, 18);
export const IconTheme = wrap(Palette, 16);
export const IconWindowMinimize = wrap(Minus, 10);
export const IconWindowMaximize = wrap(Square, 10);
export const IconWindowRestore = wrap(Copy, 10);
export const IconWindowClose = wrap(X, 10);
