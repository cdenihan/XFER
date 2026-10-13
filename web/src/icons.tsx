import type { ReactNode, SVGProps } from "react";
type Props = SVGProps<SVGSVGElement> & { size?: number };
function icon(children: ReactNode) {
  return function Icon({ size = 24, strokeWidth = 1.8, ...props }: Props) {
    return (
      <svg
        width={size}
        height={size}
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth={strokeWidth}
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
        {...props}
      >
        {children}
      </svg>
    );
  };
}
export const ArrowUpRight = icon(<path d="M6 18 18 6M6 6h12v12" />);
export const ArrowDownLeft = icon(<path d="M18 6 6 18M6 6v12h12" />);
export const ArrowRight = icon(<path d="M4 12h16m-6-6 6 6-6 6" />);
export const ChevronDown = icon(<path d="m6 9 6 6 6-6" />);
export const Monitor = icon(
  <>
    <rect x="3" y="3" width="18" height="13" rx="2" />
    <path d="M8 21h8m-4-5v5" />
  </>,
);
export const Power = icon(
  <>
    <path d="M12 2v10M6 5a9 9 0 1 0 12 0" />
  </>,
);
export const ShieldCheck = icon(
  <>
    <path d="M12 2 3 6v6c0 5 9 10 9 10s9-5 9-10V6l-9-4Z" />
    <path d="m8 12 3 3 5-6" />
  </>,
);
export const Wifi = icon(
  <>
    <path d="M2 8a17 17 0 0 1 20 0M5 12a11 11 0 0 1 14 0m-11 4a6 6 0 0 1 8 0" />
    <circle cx="12" cy="20" r=".8" fill="currentColor" stroke="none" />
  </>,
);
export const File = icon(
  <>
    <path d="M14 2H5v20h14V7l-5-5Z" />
    <path d="M14 2v5h5" />
  </>,
);
export const FilePlus2 = icon(
  <>
    <path d="M14 2H5v20h14V7l-5-5Z" />
    <path d="M14 2v5h5M8 14h8m-4-4v8" />
  </>,
);
export const Folder = icon(<path d="M2 5h8l2 3h10v12H2V5Z" />);
export const FolderPlus = icon(
  <>
    <path d="M2 5h8l2 3h10v12H2V5Z" />
    <path d="M8 14h8m-4-4v8" />
  </>,
);
export const Files = icon(
  <>
    <path d="M9 2h10v15H9V2Z" />
    <path d="M5 7H3v15h12v-2" />
  </>,
);
export const Upload = icon(<path d="M12 17V2m-5 5 5-5 5 5M3 15v7h18v-7" />);
export const Search = icon(
  <>
    <circle cx="10" cy="10" r="7" />
    <path d="m15 15 6 6" />
  </>,
);
export const X = icon(<path d="m6 6 12 12M6 18 18 6" />);
export const Check = icon(<path d="m4 12 5 5L20 6" />);
export const RefreshCw = icon(
  <>
    <path d="M20 10a8 8 0 0 0-14-5L3 8m0-5v5h5M4 14a8 8 0 0 0 14 5l3-3m0 5v-5h-5" />
  </>,
);
export const Radar = icon(
  <>
    <circle cx="12" cy="12" r="9" />
    <circle cx="12" cy="12" r="5" />
    <path d="m12 12 7-7" />
    <circle cx="12" cy="12" r="1" />
  </>,
);
export const CheckCircle2 = icon(
  <>
    <circle cx="12" cy="12" r="10" />
    <path d="m7 12 3 3 7-7" />
  </>,
);
export const LoaderCircle = icon(<path d="M21 12a9 9 0 1 1-9-9" />);
export const CircleAlert = icon(
  <>
    <circle cx="12" cy="12" r="10" />
    <path d="M12 7v6m0 4h.01" />
  </>,
);
