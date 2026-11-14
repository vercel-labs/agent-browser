import type { HTMLAttributes } from 'react'
import './Field.css'

export interface FieldProps extends Omit<HTMLAttributes<HTMLDivElement>, 'children'> {
  value: string
  placeholder?: string
  focused?: boolean
  password?: boolean
  box?: HTMLAttributes<HTMLDivElement> & Record<`data-${string}`, unknown>
}

/** Decorative typing field used by the sign-in and performance scenes. */
export function Field({ value, placeholder, focused, password, box, className, ...rest }: FieldProps) {
  return (
    <div {...rest} className={`ss-field${className ? ` ${className}` : ''}`} data-focused={focused || undefined} data-filled={value ? true : undefined}>
      <div data-cursor="ibeam" {...box} className={`ss-field__box${box?.className ? ` ${box.className}` : ''}`}>
        <span className="ss-field__text">
          {placeholder && <span className="ss-field__placeholder">{placeholder}</span>}
          <span className="ss-field__value" data-password={password || undefined}>
            <span className="ss-field__chars">{password ? '•'.repeat(value.length) : value}</span>
          </span>
          {focused && <span key={value.length} className="ss-field__caret" />}
        </span>
      </div>
    </div>
  )
}
