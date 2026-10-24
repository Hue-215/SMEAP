import { type Ref } from 'react'

interface ImageFileInputProps {
  // ボタンから開けるため受ける
  ref: Ref<HTMLInputElement>
  multiple?: boolean
  onFiles: (files: File[]) => void
}

export default function ImageFileInput({ ref, multiple, onFiles }: ImageFileInputProps) {
  return (
    <input
      ref={ref}
      type="file"
      accept="image/*"
      multiple={multiple}
      hidden
      onChange={(e) => {
        const files = Array.from(e.target.files ?? [])
        // 同じファイルを続けて選んでもchangeが起きるよう空にする
        e.target.value = ''
        if (files.length > 0) onFiles(files)
      }}
    />
  )
}
