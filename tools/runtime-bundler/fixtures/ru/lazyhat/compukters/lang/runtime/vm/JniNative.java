/*
 * The Compukters Developers
 *
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

package ru.lazyhat.compukters.lang.runtime.vm;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Comparator;

public final class JniNative {
    private JniNative() {
    }

    private static native int abiVersion();

    private static native int storeOpen(byte[] root, byte[] limits, byte[] output, long[] written);

    private static native int storeClose(long handle);

    public static void main(String[] arguments) throws Exception {
        if (arguments.length != 2) {
            throw new IllegalArgumentException("expected native library path and ABI");
        }
        System.load(Path.of(arguments[0]).toAbsolutePath().toString());
        int expected = Integer.parseUnsignedInt(arguments[1]);
        int actual = abiVersion();
        if (actual != expected) {
            throw new IllegalStateException("JNI ABI " + actual + " does not match " + expected);
        }
        System.out.println("JNI Runtime exports ABI " + actual);
        verifyJavaFileSystemRoot();
    }

    private static void verifyJavaFileSystemRoot() throws Exception {
        Path temporary = Files.createTempDirectory("compukters-jni-store ");
        try {
            Path root = Files.createDirectory(temporary.resolve("world \u043c\u0438\u0440")).toRealPath();
            long handle = openStore(root);
            try {
                byte[] reply = storeReply(root);
                if (reply.length != 2 || reply[1] != 4) {
                    throw new IllegalStateException("JNI store did not retain its exclusive lock");
                }
            } finally {
                closeStore(handle);
            }
            closeStore(openStore(root));
            Files.createDirectory(root.resolve("child"));
            byte[] rejected = storeReply(root.resolve("child").resolve(".."));
            if (rejected.length != 2 || rejected[1] != 2) {
                throw new IllegalStateException("JNI store admitted a noncanonical parent traversal");
            }
            System.out.println("JNI Java real-path store: open, lock, close, reopen and traversal rejection verified");
        } finally {
            try (var paths = Files.walk(temporary)) {
                for (Path path : paths.sorted(Comparator.reverseOrder()).toList()) {
                    Files.delete(path);
                }
            }
        }
    }

    private static byte[] storeReply(Path root) {
        byte[] output = new byte[10];
        long[] written = new long[1];
        int status = storeOpen(root.toString().getBytes(StandardCharsets.UTF_8), new byte[0], output, written);
        if (status != 0 || written[0] < 2 || written[0] > output.length || output[0] != 1) {
            throw new IllegalStateException("Invalid JNI store-open response: status=" + status);
        }
        return java.util.Arrays.copyOf(output, (int) written[0]);
    }

    private static long openStore(Path root) {
        byte[] reply = storeReply(root);
        if (reply.length != 10 || reply[1] != 0) {
            throw new IllegalStateException("JNI Java real-path store rejected with code " + reply[1] + ": " + root);
        }
        long handle = ByteBuffer.wrap(reply).order(ByteOrder.LITTLE_ENDIAN).getLong(2);
        if (handle == 0) {
            throw new IllegalStateException("JNI store returned a zero handle");
        }
        return handle;
    }

    private static void closeStore(long handle) {
        int status = storeClose(handle);
        if (status != 0) {
            throw new IllegalStateException("JNI store-close status=" + status);
        }
    }
}
