import type { WorkerRequestMessage, WorkerResponseMessage } from '../shared/types';

self.addEventListener('message', (event: MessageEvent<WorkerRequestMessage>) => {
  const { id, type, payload } = event.data;

  if (type === 'PING') {
    const response: WorkerResponseMessage = {
      id,
      type: 'PONG',
      payload: {
        timestamp: Date.now(),
        message: `Pong! Worker received payload: "${payload.message}"`
      }
    };
    self.postMessage(response);
  } else {
    const response: WorkerResponseMessage = {
      id,
      type: 'ERROR',
      payload: null,
      error: `Unknown request type: ${type}`
    };
    self.postMessage(response);
  }
});
