import { NextResponse } from "next/server";
import { deleteInactiveProxyPools, getProviderConnections } from "@/models";

// DELETE /api/proxy-pools/inactive - Delete all inactive/disabled proxy pools.
// Pools still bound to provider connections are skipped and reported instead
// of silently breaking the binding (mirrors single-delete 409 semantics).
export async function DELETE() {
  try {
    const connections = await getProviderConnections();
    const protectedIds = [];
    for (const connection of connections) {
      const proxyPoolId = connection?.providerSpecificData?.proxyPoolId;
      if (proxyPoolId) protectedIds.push(proxyPoolId);
    }

    const result = await deleteInactiveProxyPools(protectedIds);

    return NextResponse.json({
      ok: true,
      deleted: result.deletedCount,
      skippedBound: result.skippedIds.length,
      deletedIds: result.deletedIds,
      skippedIds: result.skippedIds,
    });
  } catch (error) {
    console.log("Error deleting inactive proxy pools:", error);
    return NextResponse.json({ error: "Failed to delete inactive proxy pools" }, { status: 500 });
  }
}
