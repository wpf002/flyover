using System;
using System.Collections.Generic;

namespace Demo
{
    public class App
    {
        public int Add(int a, int b)
        {
            return a + b;
        }
    }

    public interface IService
    {
        void Run();
    }

    public enum Status
    {
        Ok,
        Fail
    }
}
