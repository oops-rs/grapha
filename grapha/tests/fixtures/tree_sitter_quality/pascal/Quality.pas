unit Quality;

interface

uses Formatters;

type
  TStatus = (Ready, Stopped);

  (* Coordinates the Pascal fixture. *)
  TWorker = class(TBaseWorker, IWorker)
  private
    FOnReady: TProc;
    FLabel: string;
  public
    constructor Create;
    procedure Run;
  end;

implementation

constructor TWorker.Create;
begin
  inherited Create;
  FOnReady := @ReportReady;
  FLabel := FormatLabel('pascal');
end;

procedure TWorker.Run;
begin
  FOnReady();
end;

initialization
  TWorker.Create.Run;

end.
