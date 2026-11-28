/*
 * Andruna's access policy for servicemanager (ADR 0041).
 *
 * servicemanager asks Access (frameworks/native/cmds/servicemanager/Access.h)
 * whether the caller may find, add or list services. On Android the answer
 * comes from SELinux: the caller's security context, the service's context
 * from service_contexts, and the policy. Andruna has no SELinux and the
 * binder broker sends no security context, so this file implements the same
 * class with rules on the caller's uid, which the broker (in sysd) assigns
 * to every process it starts and which the caller cannot choose:
 *
 *   caller's app id            find   add   list
 *   below AID_APP_START        yes    yes   yes    system processes
 *   AID_APP_START and above    yes    no    no     apps, SDK sandboxes,
 *                                                  isolated processes
 *
 * ServiceManager itself still applies its own rules on top (isolated
 * callers only see services added with allowIsolated, service names are
 * validated). Distinctions SELinux makes between services (for example a
 * service only some system daemons may find) are not made: every service
 * an app can name can be found, and services check their callers' uid and
 * permissions themselves, as on Android.
 */

#include "Access.h"

#include <android-base/logging.h>
#include <binder/IPCThreadState.h>
#include <cutils/multiuser.h>
#include <private/android_filesystem_config.h>

#include <sstream>

namespace android {

namespace {

bool isSystemUid(uid_t uid) {
    return multiuser_get_app_id(uid) < AID_APP_START;
}

}  // namespace

std::string Access::CallingContext::toDebugString() const {
    std::stringstream ss;
    ss << "Caller(pid=" << debugPid << ",uid=" << uid << ",sid=" << sid << ")";
    return ss.str();
}

Access::Access() {}

Access::~Access() {}

Access::CallingContext Access::getCallingContext() {
    IPCThreadState* ipc = IPCThreadState::self();
    const char* sid = ipc->getCallingSid();
    return CallingContext{
            .debugPid = ipc->getCallingPid(),
            .uid = ipc->getCallingUid(),
            // The broker sends no security context (ADR 0025).
            .sid = sid ? std::string(sid) : std::string(),
    };
}

bool Access::canFind(const CallingContext& ctx, const std::string& name) {
    return actionAllowedFromLookup(ctx, name, "find");
}

bool Access::canAdd(const CallingContext& ctx, const std::string& name) {
    return actionAllowedFromLookup(ctx, name, "add");
}

bool Access::canList(const CallingContext& ctx) {
    return actionAllowed(ctx, nullptr, "list", "service_manager");
}

bool Access::actionAllowed(const CallingContext& sctx, const char* /*tctx*/, const char* perm,
                           const std::string& tname) {
    const std::string p = perm;
    bool allowed = p == "find" || isSystemUid(sctx.uid);
    if (!allowed) {
        LOG(WARNING) << "denied { " << perm << " } for " << sctx.toDebugString()
                     << " name=" << tname;
    }
    return allowed;
}

bool Access::actionAllowedFromLookup(const CallingContext& sctx, const std::string& name,
                                     const char* perm) {
    return actionAllowed(sctx, nullptr, perm, name);
}

}  // namespace android
