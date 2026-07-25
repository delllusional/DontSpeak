@preconcurrency import MLX

let mlxCacheLimitBytes = 256 * 1024 * 1024

struct MlxMemoryStats: Equatable {
    let activeBytes: Int
    let cacheBytes: Int
    let peakBytes: Int
    let cacheLimitBytes: Int

    func logLine(phase: String) -> String {
        "memory phase=\(phase) active_bytes=\(activeBytes) cache_bytes=\(cacheBytes) "
            + "peak_bytes=\(peakBytes) cache_limit_bytes=\(cacheLimitBytes)"
    }
}

func configureMlxMemoryPolicy() {
    Memory.cacheLimit = mlxCacheLimitBytes
}

private func currentMlxMemoryStats() -> MlxMemoryStats {
    let snapshot = Memory.snapshot()
    return MlxMemoryStats(
        activeBytes: snapshot.activeMemory,
        cacheBytes: snapshot.cacheMemory,
        peakBytes: snapshot.peakMemory,
        cacheLimitBytes: Memory.cacheLimit)
}

func logMlxMemory(phase: String) {
    logInfo(currentMlxMemoryStats().logLine(phase: phase))
}

func clearMlxCacheAndLog(phase: String) {
    Memory.clearCache()
    logMlxMemory(phase: phase)
}
