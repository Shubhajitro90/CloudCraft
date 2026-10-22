export type CloudSessionState =
  | 'unavailable'
  | 'starting'
  | 'ready'
  | 'stopping'
  | 'stopped'
  | 'error'

export interface CloudSessionRequest {
  instancePath: string
  gameVersion?: string
}

export interface CloudSessionStatus {
  state: CloudSessionState
  message: string
  sessionId?: string
  providerId?: string
  updatedAt?: string
  canStart: boolean
  canStop: boolean
}

/**
 * Provider boundary for a future remote graphical Minecraft client.
 *
 * This is deliberately not connected to a provider yet. The default
 * implementation never creates a VM, starts a remote game, or uploads saves.
 */
export interface CloudSessionProvider {
  getStatus(): Promise<CloudSessionStatus>
  startSession(request: CloudSessionRequest): Promise<CloudSessionStatus>
  stopSession(sessionId: string): Promise<CloudSessionStatus>
  cancelStart(): Promise<void>
}

class UnconfiguredCloudSessionProvider implements CloudSessionProvider {
  async getStatus(): Promise<CloudSessionStatus> {
    return {
      state: 'unavailable',
      message: 'Cloud gaming is not configured. Minecraft will run locally through the existing launcher.',
      canStart: false,
      canStop: false,
    }
  }

  async startSession(_request: CloudSessionRequest): Promise<CloudSessionStatus> {
    throw new Error('No cloud gaming provider is configured. No remote resources were created.')
  }

  async stopSession(_sessionId: string): Promise<CloudSessionStatus> {
    throw new Error('No cloud gaming provider is configured, so there is no remote session to stop.')
  }

  async cancelStart(): Promise<void> {
    // There is no configured provider and therefore no startup operation to cancel.
  }
}

export const cloudSessionProvider: CloudSessionProvider = new UnconfiguredCloudSessionProvider()
