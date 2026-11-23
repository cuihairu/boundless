// The Unreal SDK example (BW-21-04-02): a component logs in, issues an RPC,
// reads a property and logs out against a scripted server stand-in.

#include "BwSdkModule.h"

#include <cstdio>
#include <string>
#include <vector>

namespace
{
    /// A scripted server standing in for a live one: it answers the login,
    /// the RPC and the property read, and counts each call.
    class ScriptedServer : public bw::IServer
    {
    public:
        bool Login(const std::string&, const std::string&) override
        {
            logins_ = 1;
            return true;
        }

        void Logout() override { logouts_ = 1; }

        bool Rpc(long long entityId,
                 const std::string& method,
                 const std::vector<std::string>& parameters,
                 std::string& answer) override
        {
            entity_ = entityId;
            method_ = method;
            rpcs_ = static_cast<long long>(parameters.size());
            answer = "ok";
            return true;
        }

        bool GetProperty(long long entityId, const std::string& path, std::string& value) override
        {
            entity_ = entityId;
            path_ = path;
            value = "100";
            return true;
        }

        bool SyncProperty(long long entityId,
                          const std::string& path,
                          const std::string& value) override
        {
            entity_ = entityId;
            sync_path_ = path;
            sync_value_ = value;
            syncs_ = 1;
            return true;
        }

        long long logins_ = 0;
        long long logouts_ = 0;
        long long rpcs_ = 0;
        long long syncs_ = 0;
        long long entity_ = 0;
        std::string method_;
        std::string path_;
        std::string sync_path_;
        std::string sync_value_;
    };
}

int main()
{
    ScriptedServer server;
    bw::SdkComponent component(server);

    const bool logged_in = component.Login("alice", "secret");
    std::printf("LOGIN %s\n", logged_in ? "ok" : "refused");

    const bw::RpcResult result = component.Rpc(42, "heal", {"10", "20"});
    const char* answered = result.answered ? "true" : "false";
    std::printf("RPC answered=%s answer=%s\n", answered, result.answer.c_str());

    std::string value;
    const bool served = component.GetProperty(42, "hp", value);
    std::printf("PROPERTY hp=%s\n", served ? value.c_str() : "<none>");

    const bool synced = component.SyncProperty(42, "position.x", "1.5");
    std::printf("SYNC %s\n", synced ? "ok" : "refused");

    component.Logout();
    std::printf("LOGOUT ok\n");

    // The stand-in must have seen exactly one login, one logout, the RPC's
    // two parameters, the sync, its entity, the property path and the sync
    // value.
    return server.logins_ == 1 && server.logouts_ == 1 && server.rpcs_ == 2 &&
                   server.syncs_ == 1 && server.entity_ == 42 && server.method_ == "heal" &&
                   server.path_ == "hp" && server.sync_path_ == "position.x" &&
                   server.sync_value_ == "1.5"
               ? 0
               : 1;
}
