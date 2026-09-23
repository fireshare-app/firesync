/** The icon set from the design mockups, as inline stroke SVG. */

interface Props {
  size?: number
  className?: string
}

const stroke = {
  stroke: 'currentColor',
  fill: 'none',
  strokeLinecap: 'round' as const,
  strokeLinejoin: 'round' as const,
}

export const FolderIcon = ({ size = 17 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <path
      d="M2 5.2c0-.9.7-1.6 1.6-1.6h2.9l1.6 1.9h6.3c.9 0 1.6.7 1.6 1.6v5.7c0 .9-.7 1.6-1.6 1.6H3.6c-.9 0-1.6-.7-1.6-1.6z"
      {...stroke}
      strokeWidth="1.4"
    />
  </svg>
)

export const ActivityIcon = ({ size = 17 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <path d="M2 9.6h3l2-5.2 2.6 9.2 2.1-5.3 1.2 1.3H16" {...stroke} strokeWidth="1.4" />
  </svg>
)

export const GearIcon = ({ size = 17 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <circle cx="9" cy="9" r="2.5" {...stroke} strokeWidth="1.4" />
    <path
      d="M9 1.8v2M9 14.2v2M16.2 9h-2M3.8 9h-2M14.1 3.9l-1.4 1.4M5.3 12.7l-1.4 1.4M14.1 14.1l-1.4-1.4M5.3 5.3L3.9 3.9"
      {...stroke}
      strokeWidth="1.4"
    />
  </svg>
)

export const PlusIcon = ({ size = 15 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
    <path d="M8 3v10M3 8h10" {...stroke} strokeWidth="1.7" />
  </svg>
)

export const PauseIcon = ({ size = 14 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
    <path d="M6 3.5v9M10 3.5v9" {...stroke} strokeWidth="1.6" />
  </svg>
)

export const PlayIcon = ({ size = 14 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
    <path d="M5 3.4l7 4.6-7 4.6z" {...stroke} strokeWidth="1.5" />
  </svg>
)

export const PencilIcon = ({ size = 14 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
    <path d="M11.2 2.6l2.2 2.2L5.6 12.6 2.6 13.4l.8-3z" {...stroke} strokeWidth="1.4" />
  </svg>
)

export const UploadIcon = ({ size = 14 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <path d="M9 12.4V3.6M5.4 7.2L9 3.6l3.6 3.6" {...stroke} strokeWidth="1.5" />
    <path d="M3.4 13v1.2c0 .5.4.8.9.8h9.4c.5 0 .9-.3.9-.8V13" {...stroke} strokeWidth="1.5" />
  </svg>
)

export const TrashIcon = ({ size = 14 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
    <path d="M2.8 4.2h10.4M6.4 4.2V3a.8.8 0 01.8-.8h1.6a.8.8 0 01.8.8v1.2" {...stroke} strokeWidth="1.3" />
    <path d="M4.2 4.2l.6 8.2a1 1 0 001 .9h4.4a1 1 0 001-.9l.6-8.2" {...stroke} strokeWidth="1.3" />
  </svg>
)

export const RetryIcon = ({ size = 14 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
    <path d="M13.3 7a5.4 5.4 0 10-.5 3.4" {...stroke} strokeWidth="1.5" />
    <path d="M13.6 3.2v3.6h-3.6" {...stroke} strokeWidth="1.5" />
  </svg>
)

export const ArrowUpIcon = ({ size = 16 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <path d="M9 14.4V3.6M4.8 7.8L9 3.6l4.2 4.2" {...stroke} strokeWidth="1.5" />
  </svg>
)

export const ClockIcon = ({ size = 16 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <circle cx="9" cy="9" r="6.6" {...stroke} strokeWidth="1.5" />
    <path d="M9 5.2V9l2.6 1.6" {...stroke} strokeWidth="1.5" />
  </svg>
)

export const AlertCircleIcon = ({ size = 16 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <circle cx="9" cy="9" r="6.6" {...stroke} strokeWidth="1.5" />
    <path d="M9 5.4v4M9 12.1v.1" {...stroke} strokeWidth="1.6" />
  </svg>
)

export const CheckCircleIcon = ({ size = 16 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <circle cx="9" cy="9" r="6.6" {...stroke} strokeWidth="1.5" />
    <path d="M6 9.2l2.1 2.1 4-4.3" {...stroke} strokeWidth="1.6" />
  </svg>
)

export const CopyIcon = ({ size = 16 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <rect x="2.6" y="2.6" width="8.4" height="8.4" rx="1.6" {...stroke} strokeWidth="1.4" />
    <path d="M6.2 14.2c0 .7.5 1.2 1.2 1.2h6.6c.7 0 1.2-.5 1.2-1.2V7.6" {...stroke} strokeWidth="1.4" />
  </svg>
)

export const WarningTriangleIcon = ({ size = 15 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
    <path d="M8 2.2l6 11H2z" {...stroke} strokeWidth="1.4" />
    <path d="M8 6.4v3M8 11.3v.1" {...stroke} strokeWidth="1.5" />
  </svg>
)

export const DotsIcon = ({ size = 14 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
    <circle cx="8" cy="3.5" r="1.1" fill="currentColor" />
    <circle cx="8" cy="8" r="1.1" fill="currentColor" />
    <circle cx="8" cy="12.5" r="1.1" fill="currentColor" />
  </svg>
)

export const LinkIcon = ({ size = 15 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <path d="M7.4 10.6a2.9 2.9 0 0 0 4.3.3l2-2a2.9 2.9 0 0 0-4.1-4.1l-1.1 1.1" {...stroke} strokeWidth="1.4" />
    <path d="M10.6 7.4a2.9 2.9 0 0 0-4.3-.3l-2 2a2.9 2.9 0 0 0 4.1 4.1l1.1-1.1" {...stroke} strokeWidth="1.4" />
  </svg>
)

export const ExternalLinkIcon = ({ size = 15 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <path d="M10.6 2.8H15v4.4M15 2.8 8.4 9.4" {...stroke} strokeWidth="1.4" />
    <path d="M13.2 10.8v3.2c0 .7-.6 1.3-1.3 1.3H4c-.7 0-1.3-.6-1.3-1.3V6.1c0-.7.6-1.3 1.3-1.3h3.2" {...stroke} strokeWidth="1.4" />
  </svg>
)

export const CheckIcon = ({ size = 15 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <path d="m3.8 9.4 3.2 3.2 7.2-7.2" {...stroke} strokeWidth="1.6" />
  </svg>
)

export const BellIcon = ({ size = 14 }: Props) => (
  <svg width={size} height={size} viewBox="0 0 18 18" aria-hidden="true">
    <path d="M9 2.4a4.3 4.3 0 0 0-4.3 4.3c0 3.5-1.4 4.6-1.4 4.6h11.4s-1.4-1.1-1.4-4.6A4.3 4.3 0 0 0 9 2.4Z" {...stroke} strokeWidth="1.4" />
    <path d="M10.3 14a1.4 1.4 0 0 1-2.6 0" {...stroke} strokeWidth="1.4" />
  </svg>
)
