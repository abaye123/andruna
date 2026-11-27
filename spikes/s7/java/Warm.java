// A warm host stand-in: the runtime starts, then waits on stdin for the
// assignment (one line naming the class to run), then loads that class and
// runs its main method.
public class Warm {
    public static void main(String[] args) throws Exception {
        System.out.println("warm: ready");
        System.out.flush();
        StringBuilder sb = new StringBuilder();
        int c;
        while ((c = System.in.read()) != -1 && c != '\n') {
            if (c != '\r') {
                sb.append((char) c);
            }
        }
        String name = sb.length() > 0 ? sb.toString() : (args.length > 0 ? args[0] : "Hello");
        Class<?> k = Class.forName(name);
        k.getMethod("main", String[].class).invoke(null, (Object) new String[0]);
        System.out.flush();
    }
}
