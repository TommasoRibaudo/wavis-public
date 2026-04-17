import type { ChannelRole } from './channels';
import { roleBadgeColor, roleBadgeInfo } from '@shared/helpers';

interface ChannelRoleBadgeProps {
  role: ChannelRole;
  variant?: 'detail' | 'list';
  className?: string;
}

export function ChannelRoleBadge({ role, variant = 'detail', className }: ChannelRoleBadgeProps) {
  const { label, color } =
    variant === 'list'
      ? { label: role, color: roleBadgeColor(role) }
      : roleBadgeInfo(role);
  return (
    <span
      className={`px-1.5 py-0.5 border text-[0.625rem]${className ? ` ${className}` : ''}`}
      style={{ borderColor: color, color }}
    >
      {label}
    </span>
  );
}
