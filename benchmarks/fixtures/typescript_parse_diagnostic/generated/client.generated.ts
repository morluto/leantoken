export interface GeneratedClient {
  request(input: string): Promise<string>;
}

export const generatedClient: GeneratedClient = {
  async request(input) {
    return input;
  },
};
