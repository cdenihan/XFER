export type Peer = { name: string; address: string };
export type Pending = { id: number; code: string; name: string; total: number; receiving: boolean };
export type State = {
  name: string;
  output: string;
  phase: string;
  message: string;
  busy: boolean;
  bytes: number;
  total: number;
  peers: Peer[];
  pending: Pending | null;
  tailcat?: {
    available: boolean;
    active: boolean;
    enabled: boolean;
    invite: string;
    error: string;
  };
};
export type Item = { path: string; file?: File; directory?: boolean };
export type Selection = { items: Item[]; root: string; folder: boolean };
