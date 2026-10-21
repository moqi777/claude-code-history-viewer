/** The first 8 characters of a volume id: the GUID of `\\?\Volume{GUID}\` on Windows, else the id itself. */
export function shortVolumeId(uuid: string): string {
  const guid = /\{([0-9a-f-]+)\}/i.exec(uuid);
  return (guid?.[1] ?? uuid).slice(0, 8);
}
