let connection: Promise<IDBDatabase> | undefined;

function open(): Promise<IDBDatabase> {
  if (!connection) {
    connection = new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open("chat-acp-local-files", 1);
      request.onupgradeneeded = () => request.result.createObjectStore("files");
      request.onsuccess = () => {
        const db = request.result;
        db.onversionchange = () => {
          db.close();
          connection = undefined;
        };
        resolve(db);
      };
      request.onerror = () => reject(request.error);
    }).catch((error) => {
      connection = undefined;
      throw error;
    });
  }
  return connection;
}

async function transact<T>(
  mode: IDBTransactionMode,
  act: (files: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  const db = await open();
  return new Promise((resolve, reject) => {
    const tx = db.transaction("files", mode, { durability: "strict" });
    const request = act(tx.objectStore("files"));
    tx.oncomplete = () => resolve(request.result);
    tx.onabort = () => reject(tx.error);
  });
}

export function storeLocalAcpBlob(id: string, blob: Blob): Promise<IDBValidKey> {
  return transact("readwrite", (files) => files.put(blob, id));
}

export function readLocalAcpBlob(id: string): Promise<Blob | undefined> {
  return transact("readonly", (files) => files.get(id));
}

export function removeLocalAcpBlob(id: string): Promise<undefined> {
  return transact("readwrite", (files) => files.delete(id));
}
