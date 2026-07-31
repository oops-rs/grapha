unit Formatters;

interface

type
  TProc = procedure;

  TBaseWorker = class
  end;

  IWorker = interface
    procedure Run;
  end;

function FormatLabel(const Value: string): string;
procedure ReportReady;

implementation

function FormatLabel(const Value: string): string;
begin
  Result := Value;
end;

procedure ReportReady;
begin
end;

end.
