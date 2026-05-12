export type PublisherPcTrackKind = 'video' | 'audio_mic' | 'audio_sys';
export type PublisherPcOperationKind = 'publish' | 'unpublish' | 'republish';

export interface PublisherPcOperation {
  op: PublisherPcOperationKind;
  kind: PublisherPcTrackKind;
}

export interface PublisherPcTransceiver {
  slotKind: PublisherPcTrackKind;
  mediaKind: 'video' | 'audio';
  direction: RTCRtpTransceiverDirection;
  senderTrackId: string | null;
  stopped: boolean;
}

export interface PublisherPcModelStep {
  operation: PublisherPcOperation;
  reusedIndex: number | null;
  transceivers: PublisherPcTransceiver[];
  strandedSenders: number;
}

export interface PublisherPcModelState {
  transceivers: PublisherPcTransceiver[];
  strandedSenders: number;
  steps: PublisherPcModelStep[];
}

function mediaKindFor(trackKind: PublisherPcTrackKind): 'video' | 'audio' {
  return trackKind === 'video' ? 'video' : 'audio';
}

function cloneTransceiver(transceiver: PublisherPcTransceiver): PublisherPcTransceiver {
  return { ...transceiver };
}

function nextTrackId(trackKind: PublisherPcTrackKind, publishCount: number): string {
  return `${trackKind}-${publishCount}`;
}

function strandedSendersFor(transceivers: PublisherPcTransceiver[]): number {
  return transceivers.filter((transceiver) =>
    transceiver.senderTrackId === null
    && (transceiver.direction === 'sendonly' || transceiver.direction === 'sendrecv')
    && !transceiver.stopped,
  ).length;
}

function findActiveIndex(transceivers: PublisherPcTransceiver[], trackKind: PublisherPcTrackKind): number {
  return transceivers.findIndex((transceiver) =>
    transceiver.slotKind === trackKind
    && transceiver.senderTrackId !== null
    && !transceiver.stopped,
  );
}

function clearTrack(transceivers: PublisherPcTransceiver[], trackKind: PublisherPcTrackKind): PublisherPcTransceiver[] {
  return transceivers.map((transceiver) => (
    transceiver.slotKind === trackKind && !transceiver.stopped
      ? {
          ...transceiver,
          senderTrackId: null,
          direction: 'inactive',
        }
      : transceiver
  ));
}

export function applyReuse(
  transceivers: PublisherPcTransceiver[],
  trackKind: PublisherPcTrackKind,
): { transceivers: PublisherPcTransceiver[]; reusedIndex: number | null } {
  const next = transceivers.map(cloneTransceiver);
  const reusedIndex = next.findIndex((transceiver) =>
    transceiver.slotKind === trackKind
    && transceiver.direction === 'inactive'
    && transceiver.senderTrackId === null
    && !transceiver.stopped,
  );

  if (reusedIndex >= 0) {
    return {
      transceivers: next,
      reusedIndex,
    };
  }

  next.push({
    slotKind: trackKind,
    mediaKind: mediaKindFor(trackKind),
    direction: 'inactive',
    senderTrackId: null,
    stopped: false,
  });

  return {
    transceivers: next,
    reusedIndex: next.length - 1,
  };
}

export function publisherPcModel(operations: PublisherPcOperation[]): PublisherPcModelState {
  let publishCount = 0;
  let transceivers: PublisherPcTransceiver[] = [];
  const steps: PublisherPcModelStep[] = [];

  for (const operation of operations) {
    let reusedIndex: number | null = null;

    switch (operation.op) {
      case 'publish': {
        const activeIndex = findActiveIndex(transceivers, operation.kind);
        if (activeIndex >= 0) {
          publishCount += 1;
          transceivers = transceivers.map((transceiver, index) => (
            index === activeIndex
              ? {
                  ...transceiver,
                  senderTrackId: nextTrackId(operation.kind, publishCount),
                  direction: 'sendonly',
                }
              : cloneTransceiver(transceiver)
          ));
          reusedIndex = activeIndex;
          break;
        }

        const reuseResult = applyReuse(transceivers, operation.kind);
        publishCount += 1;
        reusedIndex = reuseResult.reusedIndex;
        transceivers = reuseResult.transceivers.map((transceiver, index) => (
          index === reusedIndex
            ? {
                ...transceiver,
                senderTrackId: nextTrackId(operation.kind, publishCount),
                direction: 'sendonly',
              }
            : cloneTransceiver(transceiver)
        ));
        break;
      }
      case 'unpublish': {
        transceivers = clearTrack(transceivers, operation.kind);
        break;
      }
      case 'republish': {
        transceivers = clearTrack(transceivers, operation.kind);
        const reuseResult = applyReuse(transceivers, operation.kind);
        publishCount += 1;
        reusedIndex = reuseResult.reusedIndex;
        transceivers = reuseResult.transceivers.map((transceiver, index) => (
          index === reusedIndex
            ? {
                ...transceiver,
                senderTrackId: nextTrackId(operation.kind, publishCount),
                direction: 'sendonly',
              }
            : cloneTransceiver(transceiver)
        ));
        break;
      }
    }

    const strandedSenders = strandedSendersFor(transceivers);
    steps.push({
      operation,
      reusedIndex,
      transceivers: transceivers.map(cloneTransceiver),
      strandedSenders,
    });
  }

  return {
    transceivers,
    strandedSenders: strandedSendersFor(transceivers),
    steps,
  };
}
