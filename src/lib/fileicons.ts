import {
  ArchiveIcon,
  BookIcon,
  CodeIcon,
  FileIcon,
  FileTextIcon,
  GridIcon,
  ImageIcon,
  MusicIcon,
  TerminalIcon,
  VideoIcon,
} from "svelte-feather-icons";

/** Map a file extension to a (feather icon, tailwind color) for the listing. */
export function fileType(name: string): { icon: any; color: string } {
  const ext = name.includes(".")
    ? name.slice(name.lastIndexOf(".") + 1).toLowerCase()
    : "";
  const color = "text-zinc-400";
  switch (ext) {
    case "png":
    case "jpg":
    case "jpeg":
    case "gif":
    case "svg":
    case "webp":
    case "bmp":
    case "ico":
      return { icon: ImageIcon, color: "text-emerald-400" };
    case "mp3":
    case "wav":
    case "flac":
    case "ogg":
    case "m4a":
      return { icon: MusicIcon, color: "text-fuchsia-400" };
    case "mp4":
    case "mkv":
    case "avi":
    case "mov":
    case "webm":
    case "flv":
      return { icon: VideoIcon, color: "text-rose-400" };
    case "zip":
    case "tar":
    case "gz":
    case "bz2":
    case "xz":
    case "7z":
    case "rar":
    case "tgz":
      return { icon: ArchiveIcon, color: "text-amber-400" };
    case "py":
    case "js":
    case "ts":
    case "jsx":
    case "tsx":
    case "rs":
    case "go":
    case "java":
    case "c":
    case "h":
    case "cpp":
    case "hpp":
    case "cs":
    case "rb":
    case "php":
    case "sh":
    case "bash":
    case "html":
    case "css":
    case "scss":
    case "json":
    case "yaml":
    case "yml":
    case "toml":
    case "xml":
    case "sql":
    case "vue":
    case "svelte":
    case "lua":
    case "pl":
    case "r":
      return { icon: CodeIcon, color: "text-sky-400" };
    case "txt":
    case "md":
    case "log":
    case "ini":
    case "conf":
    case "cfg":
      return { icon: FileTextIcon, color: "text-zinc-400" };
    case "pdf":
      return { icon: BookIcon, color: "text-red-400" };
    case "doc":
    case "docx":
    case "odt":
    case "rtf":
      return { icon: FileTextIcon, color: "text-blue-400" };
    case "xls":
    case "xlsx":
    case "csv":
    case "ods":
      return { icon: GridIcon, color: "text-green-500" };
    case "exe":
    case "msi":
    case "bin":
    case "so":
    case "dll":
      return { icon: TerminalIcon, color: "text-purple-400" };
    default:
      return { icon: FileIcon, color };
  }
}
