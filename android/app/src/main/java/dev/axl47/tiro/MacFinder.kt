package dev.axl47.tiro

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import java.net.Inet4Address
import java.util.concurrent.CountDownLatch
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * Finds the paired Mac again when its address changed: the Mac advertises
 * `_tiro._tcp` over Bonjour with its certificate's fingerprint
 * (`src/lan/bonjour.rs`). The fingerprint only picks which Mac; the
 * connection still pins the certificate, so a lying advert gets nowhere.
 */
object MacFinder {
    private const val SERVICE_TYPE = "_tiro._tcp."
    private const val SEARCH_MS = 4_000L

    /**
     * Runs [work] against [pairing]'s Mac. When the Mac can't be reached,
     * looks for it on the network, saves the address it's at now, and runs
     * [work] once more there. Blocking.
     */
    fun <T> withMac(context: Context, pairing: Pairing, work: (Pairing) -> T): T = try {
        work(pairing)
    } catch (failure: MacClient.Failure) {
        if (failure.code != MacClient.UNREACHABLE) throw failure
        val moved = find(context, pairing) ?: throw failure
        PairingStore.save(context, moved)
        android.util.Log.i(TAG, "found the Mac at ${moved.host}:${moved.port}")
        work(moved)
    }

    /** [pairing] at the address its Mac advertises now; null if it isn't found, or hasn't moved. */
    fun find(context: Context, pairing: Pairing): Pairing? {
        val nsd = context.getSystemService(NsdManager::class.java) ?: return null
        val seen = LinkedBlockingQueue<NsdServiceInfo>()
        val discovery = object : NsdManager.DiscoveryListener {
            override fun onServiceFound(service: NsdServiceInfo) {
                seen.add(service)
            }
            override fun onServiceLost(service: NsdServiceInfo) = Unit
            override fun onDiscoveryStarted(serviceType: String) = Unit
            override fun onDiscoveryStopped(serviceType: String) = Unit
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) = Unit
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) = Unit
        }
        nsd.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, discovery)
        try {
            val deadline = System.currentTimeMillis() + SEARCH_MS
            while (true) {
                val left = deadline - System.currentTimeMillis()
                if (left <= 0) return null
                val service = seen.poll(left, TimeUnit.MILLISECONDS) ?: return null
                val resolved = resolve(nsd, service, left) ?: continue
                val fingerprint = resolved.attributes["fp"]?.toString(Charsets.UTF_8)
                if (fingerprint != pairing.fingerprint) continue
                val host = address(resolved) ?: continue
                if (host == pairing.host && resolved.port == pairing.port) return null
                return pairing.copy(host = host, port = resolved.port)
            }
        } finally {
            runCatching { nsd.stopServiceDiscovery(discovery) }
        }
    }

    @Suppress("DEPRECATION") // resolveService is the one way that works on API 29 to 33 too.
    private fun resolve(nsd: NsdManager, service: NsdServiceInfo, timeoutMs: Long): NsdServiceInfo? {
        var resolved: NsdServiceInfo? = null
        val done = CountDownLatch(1)
        nsd.resolveService(service, object : NsdManager.ResolveListener {
            override fun onServiceResolved(info: NsdServiceInfo) {
                resolved = info
                done.countDown()
            }
            override fun onResolveFailed(info: NsdServiceInfo, errorCode: Int) {
                done.countDown()
            }
        })
        done.await(timeoutMs, TimeUnit.MILLISECONDS)
        return resolved
    }

    /** The Mac's IPv4 address when it has one: what the pairing link carries. */
    @Suppress("DEPRECATION")
    private fun address(info: NsdServiceInfo): String? {
        val addresses = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            info.hostAddresses
        } else {
            listOfNotNull(info.host)
        }
        return (addresses.firstOrNull { it is Inet4Address } ?: addresses.firstOrNull())?.hostAddress
    }

    private const val TAG = "TiroFinder"
}
