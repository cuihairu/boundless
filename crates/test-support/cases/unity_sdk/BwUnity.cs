namespace Bw
{
    using System;
    using System.Collections.Generic;
    using UnityEngine;

    /// One entity lifecycle event the binding reports to Unity.
    public sealed class BwEntityEvent
    {
        /// The entity the event is about.
        public long EntityId { get; }

        /// The event's kind: created, entered, left or destroyed.
        public string Kind { get; }

        public BwEntityEvent(long entityId, string kind)
        {
            EntityId = entityId;
            Kind = kind;
        }
    }

    /// A server the Unity binding talks to.  A test stands in for a live
    /// server with a scripted object, so the binding's shape is checkable
    /// without the server.
    public interface IBwServer
    {
        bool Login(string user, string password);
        void Logout();
        BwEntityEvent? NextEntityEvent();
        void SendMovement(long entityId, double x, double y, double z, long timestamp);
        long Rpc(long entityId, string method, string[] parameters);
        bool Answer(long callId, string[] values);
        string? GetProperty(long entityId, string path);
        bool SetProperty(long entityId, string path, string value);
    }

    /// The SDK's Unity binding: the C# view of the SDK, a component the
    /// editor loads.  Events posted from a worker thread are queued and
    /// delivered by [`Pump`], which the Unity main thread calls each frame,
    /// so every callback runs on the engine's main thread.
    public sealed class BwUnity : MonoBehaviour
    {
        /// The server this binding talks to, set by the host component before
        /// the first login.
        public IBwServer Server { get; set; } = null!;

        /// Whether the binding is logged in.
        public bool LoggedIn { get; private set; }

        private readonly List<BwEntityEvent> queued = new();

        /// Log in; a refused login leaves the binding logged out.
        public bool Login(string user, string password)
        {
            if (LoggedIn)
            {
                return false;
            }

            if (!Server.Login(user, password))
            {
                return false;
            }

            LoggedIn = true;
            return true;
        }

        /// Log out; a logged-out binding logs out again without a server call.
        public void Logout()
        {
            if (!LoggedIn)
            {
                return;
            }

            Server.Logout();
            LoggedIn = false;
        }

        /// Post one entity event from a worker thread; [`Pump`] delivers it
        /// on the main thread.
        public void Queue(BwEntityEvent entityEvent)
        {
            queued.Add(entityEvent);
        }

        /// Deliver every posted event on the main thread, in order, and report
        /// how many ran.  Unity calls it from the component's per-frame
        /// update.
        public int Pump()
        {
            int delivered = 0;
            foreach (BwEntityEvent entityEvent in queued)
            {
                Deliver(entityEvent);
                delivered++;
            }

            queued.Clear();
            return delivered;
        }

        /// Deliver one event: the engine's log is the stand-in for a game
        /// handler.
        public void Deliver(BwEntityEvent entityEvent)
        {
            Debug.Log($"entity {entityEvent.EntityId} {entityEvent.Kind}");
        }

        /// Send movement input; a logged-out binding refuses.
        public void SendMovement(long entityId, double x, double y, double z, long timestamp)
        {
            RequireLogin();
            Server.SendMovement(entityId, x, y, z, timestamp);
        }

        /// Make an RPC call and return its correlation id.
        public long Rpc(long entityId, string method, string[] parameters)
        {
            RequireLogin();
            return Server.Rpc(entityId, method, parameters);
        }

        /// Answer an RPC call by its correlation id.
        public bool Answer(long callId, string[] values)
        {
            RequireLogin();
            return Server.Answer(callId, values);
        }

        /// Read one property by its entity and path.
        public string? GetProperty(long entityId, string path)
        {
            return Server.GetProperty(entityId, path);
        }

        /// Write one property by its entity and path.
        public bool SetProperty(long entityId, string path, string value)
        {
            return Server.SetProperty(entityId, path, value);
        }

        private void RequireLogin()
        {
            if (!LoggedIn)
            {
                throw new InvalidOperationException("not logged in");
            }
        }
    }
}
