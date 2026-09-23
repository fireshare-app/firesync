const MB = 1024 * 1024

/** A transfer rate somebody can read at a glance. */
export function humanRate(bytesPerSecond: number) {
  if (bytesPerSecond >= MB) return `${(bytesPerSecond / MB).toFixed(1)} MB/s`
  if (bytesPerSecond >= 1024) return `${Math.round(bytesPerSecond / 1024)} KB/s`
  return `${Math.round(bytesPerSecond)} B/s`
}
