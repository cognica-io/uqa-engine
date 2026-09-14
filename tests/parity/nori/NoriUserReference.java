// Unified Query Algebra
// Copyright (c) 2023-2026 Cognica, Inc.

import java.io.StringReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Base64;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.apache.lucene.analysis.ko.dict.KoMorphData;
import org.apache.lucene.analysis.ko.dict.UserDictionary;
import org.apache.lucene.util.fst.FST;

/** Public-API snapshots of user-rule compilation and every matched prefix. */
public class NoriUserReference {
  static Map<String, Object> object(Object... pairs) {
    Map<String, Object> value = new LinkedHashMap<>();
    for (int i = 0; i < pairs.length; i += 2) value.put((String) pairs[i], pairs[i + 1]);
    return value;
  }

  static List<Integer> units(String text) {
    List<Integer> value = new ArrayList<>();
    for (char unit : text.toCharArray()) value.add((int) unit);
    return value;
  }

  static String json(Object value) {
    if (value == null) return "null";
    if (value instanceof Number || value instanceof Boolean) return value.toString();
    if (value instanceof Map<?, ?> map) {
      List<String> fields = new ArrayList<>();
      map.forEach((key, item) -> fields.add(json(key) + ":" + json(item)));
      return "{" + String.join(",", fields) + "}";
    }
    if (value instanceof List<?> list) {
      return "[" + String.join(",", list.stream().map(NoriUserReference::json).toList()) + "]";
    }
    StringBuilder text = new StringBuilder("\"");
    for (char unit : value.toString().toCharArray()) {
      if (unit == '"' || unit == '\\') text.append('\\').append(unit);
      else if (unit < 32 || unit == 0x85 || unit == 0x2028 || unit == 0x2029 || Character.isSurrogate(unit)) text.append(String.format("\\u%04x", (int) unit));
      else text.append(unit);
    }
    return text.append('"').toString();
  }

  static Map<String, Object> snapshot(String id, String rules, String query) throws Exception {
    var result = object("id", id, "rules", rules, "query", query);
    try {
      UserDictionary dictionary = UserDictionary.open(new StringReader(rules));
      result.put("empty", dictionary == null);
      List<Object> matches = new ArrayList<>();
      if (dictionary != null) {
        KoMorphData data = dictionary.getMorphAttributes();
        var fst = dictionary.getFST();
        var reader = fst.getBytesReader();
        char[] chars = query.toCharArray();
        for (int start = 0; start < chars.length; start++) {
          var arc = fst.getFirstArc(new FST.Arc<Long>());
          int output = 0;
          for (int end = start; end < chars.length; end++) {
            if (fst.findTargetArc(chars[end], arc, arc, end == start, reader) == null) break;
            output += arc.output().intValue();
            if (!arc.isFinal()) continue;
            int word = output + arc.nextFinalOutput().intValue();
            var parts = data.getMorphemes(word, chars, start, end - start + 1);
            List<Object> morphemes = null;
            if (parts != null) {
              morphemes = new ArrayList<>();
              for (var part : parts) morphemes.add(object("surface_utf16", units(part.surfaceForm()), "pos", part.posTag()));
            }
            matches.add(object("start", start, "end", end + 1, "id", word,
                "left", data.getLeftId(word), "right", data.getRightId(word), "cost", data.getWordCost(word),
                "pos_type", data.getPOSType(word), "left_pos", data.getLeftPOS(word), "right_pos", data.getRightPOS(word),
                "reading", data.getReading(word), "morphemes", morphemes));
          }
        }
        List<Integer> expectedIds = dictionary.lookup(chars, 0, chars.length);
        var actualIds = matches.stream().map(match -> ((Map<?, ?>) match).get("id")).toList();
        if (!expectedIds.equals(actualIds)) throw new IllegalStateException("Prefix traversal differs from public lookup");
      }
      result.put("matches", matches);
    } catch (IllegalArgumentException error) {
      result.put("error", error.getClass().getName());
    }
    return result;
  }

  public static void main(String[] args) throws Exception {
    System.out.println(json(object("runtime", object("java_version", System.getProperty("java.version"),
        "java_runtime_version", System.getProperty("java.runtime.version"), "java_vendor", System.getProperty("java.vendor")))));
    for (String row : Files.readAllLines(Path.of(args[0]), StandardCharsets.UTF_8)) {
      String[] fields = row.split("\t", -1);
      String rules = new String(Base64.getDecoder().decode(fields[1]), StandardCharsets.UTF_8);
      String query = new String(Base64.getDecoder().decode(fields[2]), StandardCharsets.UTF_8);
      System.out.println(json(snapshot(fields[0], rules, query)));
    }
  }
}
