import { useNavigate } from 'react-router';
import { CmdButton } from '@shared/CmdButton';
import { EmptyState } from '@shared/EmptyState';

/**
 * LegacyRoom — Direct room create/join (non-channel backward compat path).
 */
export default function LegacyRoom() {
  const navigate = useNavigate();

  // TODO: Create room form (room ID, display name)
  // Join room form (room ID, invite code, display name)
  // Invite management (create, revoke, list)

  return (
    <div className="h-full flex flex-col bg-wavis-bg font-mono text-wavis-text">
      <div className="flex-1 overflow-y-auto">
        <div className="max-w-2xl mx-auto px-6 py-6">
          <div className="mb-4">
            <CmdButton label="← /back" onClick={() => navigate('/')} />
          </div>
          <h2>legacy room</h2>
          <div className="text-wavis-text-secondary my-4">{'─'.repeat(48)}</div>
          <EmptyState message="legacy room not yet implemented" className="p-4" />
        </div>
      </div>

      {/* Bottom command bar */}
      <div className="border-t border-wavis-text-secondary px-6 py-3 flex items-center gap-6">
        <CmdButton label="/create" onClick={() => {}} />
        <CmdButton label="/join" onClick={() => {}} />
        <CmdButton label="/back" onClick={() => navigate('/')} />
      </div>
    </div>
  );
}
